use super::*;
use crate::{
    constants::{INTEREST_INITIAL_RATE_AT_TARGET_NAD, MARKET_LAYOUT_VERSION, MIN_HALF_LIFE_MS, NAD},
    instructions::SwapRequest,
    state::{
        AmmConfig, Debt, HlpVault, Insurance, MarketConfig, MarketSide, ProtocolAuctionSplit, ReserveShares, Reserves,
        Risk,
    },
};

fn test_market(base_cash: u64, quote_cash: u64) -> Market {
    let mut base_side = MarketSide::default();
    base_side.reserves = Reserves {
        live_reserve: base_cash,
        cash_reserve: base_cash,
    };
    base_side.shares = ReserveShares { ylp_supply: base_cash };
    let mut quote_side = MarketSide::default();
    quote_side.reserves = Reserves {
        live_reserve: quote_cash,
        cash_reserve: quote_cash,
    };
    quote_side.shares = ReserveShares { ylp_supply: quote_cash };
    Market {
        version: MARKET_LAYOUT_VERSION,
        ylp_mint: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: MarketConfig {
            swap_fee_bps: 0,
            divergence_fee_share_cap_bps: 2_000,
            volatility_fee_share_cap_bps: 2_000,
            max_daily_borrow_bps: 3_000,
            ..MarketConfig::default()
        },
        amm: Default::default(),
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            ..Debt::default()
        },
        base_hlp_vault: HlpVault::default(),
        quote_hlp_vault: HlpVault::default(),
        risk: Risk::default(),
        insurance: Insurance::default(),
        params_hash: [0u8; 32],
        governance_locked_ylp: 0,
        parameter_revisions: [0; 5],
        last_marginal_observation_nad: 0,
        curve_revision: 0,
        risk_revision: 0,
        last_update_slot: 0,
        reduce_only: false,
        bump: 255,
    }
}

fn empty_position() -> LeveragePosition {
    LeveragePosition {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        position_id: Pubkey::default(),
        referral_partner: Pubkey::default(),
        referral_interest_share_bps: 0,
        debt_asset: 0,
        collateral_amount: 0,
        margin_amount: 0,
        open_notional: 0,
        debt_principal: 0,
        debt_shares: 0,
        multiplier_bps: 0,
        opened_at: 0,
        opened_slot: 0,
        bump: 0,
    }
}

fn seeded_position(
    market: &mut Market,
    debt_asset: MarketAsset,
    debt_amount: u64,
    collateral_amount: u64,
) -> LeveragePosition {
    let debt_shares = market.debt.add_isolated_debt(debt_asset, debt_amount).unwrap();
    match debt_asset {
        MarketAsset::Base => {
            market.base_side.reserves.cash_reserve -= debt_amount;
        }
        MarketAsset::Quote => {
            market.quote_side.reserves.cash_reserve -= debt_amount;
        }
    }
    let mut position = empty_position();
    position.initialize(
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::default(),
        0,
        debt_asset,
        collateral_amount,
        debt_amount,
        debt_amount * 2,
        debt_amount,
        debt_shares,
        20_000,
        0,
        0,
        255,
    );
    position
}

fn full_fee_credit(quote: &LeverageSwapQuote) -> LeverageSwapFeeCredit {
    LeverageSwapFeeCredit::from_total_actual_credit(quote, quote.fee_credit).unwrap()
}

fn prepared_leverage_swap(market: &Market, swap: LeverageSwapQuote) -> PreparedLeverageSwap {
    PreparedLeverageSwap {
        swap,
        base_pre_rebalance: HlpRebalanceReceipt::default(),
        quote_pre_rebalance: HlpRebalanceReceipt {
            target_asset: MarketAsset::Quote,
            ..HlpRebalanceReceipt::default()
        },
        fee_eligible_ylp_supply: market
            .side(MarketAsset::try_from_code(swap.asset_in).unwrap())
            .shares
            .ylp_supply,
        interest_eligibility: HlpYieldEligibility {
            ylp_supply: market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: market.quote_hlp_vault.ylp_shares,
        },
    }
}

fn concentrated_market() -> Market {
    let mut market = test_market(1_000_000, 1_000_000);
    market.config.swap_fee_bps = 30;
    market.config.divergence_fee_share_cap_bps = 2_000;
    market.config.volatility_fee_share_cap_bps = 2_000;
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        center_ema_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_shock_cap_nad: NAD / 10,
        volatility_cap_nad: NAD,
        divergence_fee_coefficient_nad: 10 * NAD,
        volatility_fee_coefficient_nad: NAD / 10,
        ..AmmConfig::default()
    };
    market.checkpoint_amm_neutral_inventory(1).unwrap();
    market
}

fn active_concentrated_hlp_market() -> Market {
    let mut market = concentrated_market();
    market.config.target_hlp_leverage_bps = 20_000;
    // Lifecycle success fixtures intentionally use the widest valid band. A
    // separate test below proves that worsening flow outside a narrow band is
    // rejected.
    market.config.settlement_divergence_bps = 10_000;
    market.deposit_single_sided(MarketAsset::Base, 100_000, 1).unwrap();
    assert!(market.base_hlp_vault.hlp_supply > 0);
    assert!(!market.current_curve_parameters(1).is_cpmm());
    market
}

fn assert_exact_concentrated_hlp_residual_exposure(market: &Market) {
    let base_residual_exposure = market.base_hlp_vault.residual_exposure;
    let quote_residual_exposure = market.quote_hlp_vault.residual_exposure;

    let mut independently_checkpointed = market.clone();
    let (expected_base, expected_quote) = independently_checkpointed.checkpoint_hlp_vaults().unwrap();
    assert_eq!(base_residual_exposure, expected_base);
    assert_eq!(quote_residual_exposure, expected_quote);
}

fn assert_final_leverage_risk_observation(market: &Market, current_slot: u64, revision_before: u64) {
    let final_evaluation = market.evaluate_current_curve(current_slot).unwrap();
    let final_price_nad = u64::try_from(final_evaluation.marginal_price_nad).unwrap();

    assert_eq!(market.curve_revision, revision_before + 1);
    assert_eq!(market.risk_revision, revision_before);
    assert_eq!(market.risk.last_snapshot_slot, current_slot);
    assert_eq!(market.risk.cached_spot_base_price_nad, final_price_nad);
    assert_eq!(market.last_marginal_observation_nad, final_price_nad);
    assert_eq!(market.amm.invariant_d_nad, final_evaluation.invariant_d);
}

#[test]
fn open_leverage_tracks_isolated_debt_and_cash() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let quote = market.quote_leverage_swap(MarketAsset::Base, 2_000, 1).unwrap();
    let fee_credit = full_fee_credit(&quote);

    let receipt = market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            1_000,
            20_000,
            quote.amount_out,
            prepared_leverage_swap(&market, quote),
            fee_credit,
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_eq!(receipt.debt_amount, 1_000);
    assert_eq!(position.debt_shares, 1_000);
    assert_eq!(market.debt.isolated_base_shares, 1_000);
    assert_eq!(market.debt.fixed_base_shares, 0);
    assert_eq!(
        market.base_side.reserves.live_reserve,
        1_000_000 + quote.reserve_input_credit
    );
    assert_eq!(
        market.base_side.reserves.cash_reserve,
        1_000_000 - 1_000 + quote.reserve_input_credit
    );
    assert_eq!(market.quote_side.reserves.live_reserve, 1_000_000 - quote.amount_out);
    assert_eq!(market.quote_side.reserves.cash_reserve, 1_000_000 - quote.amount_out);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn referred_leverage_records_exact_debt_and_binds_partner() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let referral_partner = Pubkey::new_unique();
    let open_quote = market.quote_leverage_swap(MarketAsset::Base, 2_000, 1).unwrap();
    let open_fee_credit = full_fee_credit(&open_quote);

    let open = market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            referral_partner,
            2_500,
            MarketAsset::Base,
            1_000,
            20_000,
            open_quote.amount_out,
            prepared_leverage_swap(&market, open_quote),
            open_fee_credit,
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_eq!(open.borrowed_amount, 1_000);
    assert_eq!(open.debt_amount, 1_000);
    assert_eq!(open.swap.amount_in, 2_000);
    assert_eq!(position.referral_partner, referral_partner);
    assert_eq!(position.referral_interest_share_bps, 2_500);
    assert_eq!(position.debt_principal, 1_000);
    assert_eq!(market.debt.isolated_base_principal, 1_000);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 0);

    let increase_quote = market.quote_leverage_swap(MarketAsset::Base, 100, 2).unwrap();
    let increase_fee_credit = full_fee_credit(&increase_quote);
    let increase = market
        .increase_leverage(
            &mut position,
            100,
            increase_quote.amount_out,
            prepared_leverage_swap(&market, increase_quote),
            increase_fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            2,
        )
        .unwrap();
    assert_eq!(increase.borrowed_amount, 100);
    assert_eq!(increase.debt_delta, 100);
    assert_eq!(position.referral_partner, referral_partner);
    assert_eq!(position.referral_interest_share_bps, 2_500);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 0);
}

#[test]
fn remove_leverage_margin_does_not_consume_the_public_borrow_bucket() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 100, 1_000);

    let receipt = market.remove_leverage_margin(&mut position, 10, 0).unwrap();

    assert_eq!(receipt.borrowed_amount, 10);
    assert_eq!(receipt.debt_delta, 10);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 0);
}

#[test]
fn isolated_leverage_ignores_capacity_used_by_public_borrowers() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 100, 1_000);
    let remaining_for_isolated = 9;
    let limit = market
        .daily_limit_for_side(MarketAsset::Base, market.config.max_daily_borrow_bps)
        .unwrap();
    market
        .side_mut(MarketAsset::Base)
        .daily_borrow_bucket
        .record_borrow(limit - remaining_for_isolated, limit, 0)
        .unwrap();

    let receipt = market.remove_leverage_margin(&mut position, 10, 0).unwrap();

    assert_eq!(receipt.borrowed_amount, 10);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, limit - 9);
}

#[test]
fn close_leverage_clears_isolated_debt_and_residual_cash() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = empty_position();
    let open_quote = market.quote_leverage_swap(MarketAsset::Base, 2_000, 1).unwrap();
    let open_fee_credit = full_fee_credit(&open_quote);
    market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            1_000,
            20_000,
            open_quote.amount_out,
            prepared_leverage_swap(&market, open_quote),
            open_fee_credit,
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    let base_cash_before_close = market.base_side.reserves.cash_reserve;
    let close_quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 2)
        .unwrap();
    let close_fee_credit = full_fee_credit(&close_quote);

    let receipt = market
        .close_leverage(
            &mut position,
            0,
            prepared_leverage_swap(&market, close_quote),
            close_fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            2,
        )
        .unwrap();

    assert_eq!(receipt.debt_repaid, 1_000);
    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(market.debt.isolated_base_principal, 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert_eq!(
        market.base_side.reserves.cash_reserve,
        base_cash_before_close - receipt.residual
    );
    assert_eq!(receipt.closeout_value, close_quote.amount_out);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn add_margin_never_reduces_more_debt_than_the_cash_repaid() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 100, 10_000);
    market.debt.base_borrow_index_nad = (NAD as u128) * 3 / 2;
    market.base_side.reserves.live_reserve += 50;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    let live_before = market.base_side.reserves.live_reserve;
    let cash_before = market.base_side.reserves.cash_reserve;

    let receipt = market.add_leverage_margin(&mut position, 2, 1).unwrap();

    assert_eq!(receipt.debt_delta, -2);
    assert_eq!(receipt.debt_amount, 148);
    assert_eq!(receipt.interest_paid, 1);
    assert_eq!(position.debt_shares, 99);
    assert_eq!(position.debt_principal, 99);
    assert_eq!(market.debt.isolated_base_shares, 99);
    assert_eq!(market.debt.isolated_base_principal, 99);
    assert_eq!(market.base_side.reserves.live_reserve, live_before - 1);
    assert_eq!(market.base_side.reserves.cash_reserve, cash_before + 1);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn solvent_liquidation_closes_position_and_pays_residual_incentive() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 1_010);
    let quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 1)
        .unwrap();
    let fee_credit = full_fee_credit(&quote);

    let receipt = market
        .liquidate_leverage(
            &mut position,
            prepared_leverage_swap(&market, quote),
            fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert_eq!(receipt.debt_repaid, 1_000);
    assert_eq!(receipt.principal_written_off, 0);
    assert!(receipt.liquidator_amount > 0);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn insolvent_liquidation_socializes_unrepaid_principal() {
    let mut market = test_market(1_000_000, 1_000_000);
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 500);
    let quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 1)
        .unwrap();
    let fee_credit = full_fee_credit(&quote);

    let receipt = market
        .liquidate_leverage(
            &mut position,
            prepared_leverage_swap(&market, quote),
            fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert_eq!(market.debt.isolated_base_shares, 0);
    assert_eq!(position.debt_shares, 0);
    assert_eq!(position.collateral_amount, 0);
    assert!(receipt.debt_repaid < 1_000);
    assert!(receipt.principal_written_off > 0);
    assert_eq!(receipt.liquidator_amount, 0);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn leverage_quote_is_the_same_concentrated_and_dynamic_fee_quote_as_spot() {
    let market = concentrated_market();
    let leverage = market.quote_leverage_swap(MarketAsset::Base, 50_000, 1).unwrap();
    let spot = market.quote_amm_swap(MarketAsset::Base, 50_000, 1).unwrap();

    assert_eq!(leverage.amount_out, spot.amount_out);
    assert_eq!(leverage.amount_in_after_fee, spot.fee.amount_in_for_quote);
    assert_eq!(leverage.reserve_input_credit, spot.fee.reserve_input_credit);
    assert_eq!(leverage.end_price_nad, spot.end_price_nad);
    assert_eq!(leverage.reserve_end_price_nad, spot.reserve_end_price_nad);
    assert_eq!(leverage.fee_breakdown, spot.fee);
    assert!(leverage.fee_breakdown.divergence_surcharge_debit > 0);
}

#[test]
fn concentrated_open_leverage_checkpoints_active_hlp_exposure() {
    let mut market = active_concentrated_hlp_market();
    let mut position = empty_position();
    let quote = market.quote_leverage_swap(MarketAsset::Base, 2_000, 1).unwrap();
    let revision_before = market.curve_revision;

    market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            1_000,
            20_000,
            quote.amount_out,
            prepared_leverage_swap(&market, quote),
            full_fee_credit(&quote),
            0,
            1,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    assert_exact_concentrated_hlp_residual_exposure(&market);
    assert_final_leverage_risk_observation(&market, 1, revision_before);
}

#[test]
fn concentrated_increase_leverage_checkpoints_active_hlp_exposure() {
    let mut market = active_concentrated_hlp_market();
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 10_000);
    let quote = market.quote_leverage_swap(MarketAsset::Base, 100, 1).unwrap();
    let revision_before = market.curve_revision;

    market
        .increase_leverage(
            &mut position,
            100,
            quote.amount_out,
            prepared_leverage_swap(&market, quote),
            full_fee_credit(&quote),
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert_exact_concentrated_hlp_residual_exposure(&market);
    assert_final_leverage_risk_observation(&market, 1, revision_before);
}

#[test]
fn concentrated_decrease_leverage_checkpoints_active_hlp_exposure() {
    let mut market = active_concentrated_hlp_market();
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 10_000);
    let quote = market.quote_leverage_swap(MarketAsset::Quote, 100, 1).unwrap();
    let revision_before = market.curve_revision;

    market
        .decrease_leverage(
            &mut position,
            100,
            0,
            prepared_leverage_swap(&market, quote),
            full_fee_credit(&quote),
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert_exact_concentrated_hlp_residual_exposure(&market);
    assert_final_leverage_risk_observation(&market, 1, revision_before);
}

#[test]
fn concentrated_close_leverage_checkpoints_active_hlp_exposure() {
    let mut market = active_concentrated_hlp_market();
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 2_000);
    let quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 1)
        .unwrap();
    let revision_before = market.curve_revision;

    market
        .close_leverage(
            &mut position,
            0,
            prepared_leverage_swap(&market, quote),
            full_fee_credit(&quote),
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert_exact_concentrated_hlp_residual_exposure(&market);
    assert_final_leverage_risk_observation(&market, 1, revision_before);
}

#[test]
fn concentrated_liquidation_checkpoints_active_hlp_exposure() {
    let mut market = active_concentrated_hlp_market();
    let mut position = seeded_position(&mut market, MarketAsset::Base, 1_000, 900);
    let quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 1)
        .unwrap();
    let revision_before = market.curve_revision;

    let receipt = market
        .liquidate_leverage(
            &mut position,
            prepared_leverage_swap(&market, quote),
            full_fee_credit(&quote),
            0,
            ProtocolAuctionSplit::default(),
            1,
        )
        .unwrap();

    assert!(receipt.principal_written_off > 0);
    assert_exact_concentrated_hlp_residual_exposure(&market);
    assert_final_leverage_risk_observation(&market, 1, revision_before);
}

#[test]
fn next_risk_refresh_integrates_the_post_leverage_mark() {
    let mut market = concentrated_market();
    market.config.ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.directional_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.config.q_ema_half_life_ms = MIN_HALF_LIFE_MS;
    market.observe_current_risk(1).unwrap();
    let risk_before_leverage = market.risk;
    let mut position = empty_position();
    let quote = market.quote_leverage_swap(MarketAsset::Base, 20_000, 2).unwrap();

    market
        .open_leverage(
            &mut position,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::default(),
            0,
            MarketAsset::Base,
            10_000,
            20_000,
            quote.amount_out,
            prepared_leverage_swap(&market, quote),
            full_fee_credit(&quote),
            0,
            2,
            255,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();

    let post_leverage_mark = market.risk.cached_spot_base_price_nad;
    assert_ne!(post_leverage_mark, risk_before_leverage.cached_spot_base_price_nad);
    assert_eq!(market.risk.last_snapshot_slot, 2);
    assert!(market.curve_revision > market.risk_revision);

    let mut expected = market.clone();
    expected.observe_current_risk(3).unwrap();

    let mut stale_pre_leverage_observation = market.clone();
    stale_pre_leverage_observation.risk = risk_before_leverage;
    stale_pre_leverage_observation.observe_current_risk(3).unwrap();
    assert_ne!(
        expected.risk.base_price_ema_nad,
        stale_pre_leverage_observation.risk.base_price_ema_nad
    );

    market.observe_current_risk(3).unwrap();
    assert_eq!(market.risk, expected.risk);
    assert_eq!(market.risk_revision, market.curve_revision);
}

#[test]
fn concentrated_leverage_rejects_worsening_stale_hlp_exposure() {
    let mut market = active_concentrated_hlp_market();
    market.config.settlement_divergence_bps = 1;
    let stale_trade = market.quote_curve_exact_in(MarketAsset::Base, 150_000, 1).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            150_000,
            stale_trade.amount_out,
            0,
            0,
            ProtocolAuctionSplit::default(),
        )
        .unwrap();
    market.checkpoint_amm_neutral_inventory(1).unwrap();
    market.checkpoint_hlp_vaults().unwrap();
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
    let error = SwapRequest {
        current_slot: 1,
        asset_in: MarketAsset::Base,
        reserve_credit: 50_000,
    }
    .prepare(&mut market)
    .unwrap_err();

    assert_eq!(error, error!(ErrorCode::HlpSettlementUnavailable));
}

#[test]
fn leverage_execution_rejects_a_tampered_internal_quote() {
    let mut market = concentrated_market();
    let quote = market.quote_leverage_swap(MarketAsset::Base, 50_000, 1).unwrap();

    let mut wrong_direction = quote;
    wrong_direction.asset_in = MarketAsset::Quote.code();
    assert!(market
        .validate_leverage_swap_quote(wrong_direction, MarketAsset::Base, 1)
        .is_err());

    let mut stale_slot = quote;
    stale_slot.quoted_slot = 2;
    assert!(market
        .validate_leverage_swap_quote(stale_slot, MarketAsset::Base, 1)
        .is_err());

    let mut broken_fee_identity = quote;
    broken_fee_identity.reserve_input_credit += 1;
    assert!(market
        .validate_leverage_swap_quote(broken_fee_identity, MarketAsset::Base, 1)
        .is_err());

    market.amm.retain_dynamic_surcharge = !market.amm.retain_dynamic_surcharge;
    assert!(market
        .validate_leverage_swap_quote(quote, MarketAsset::Base, 1)
        .is_err());
}

#[test]
fn leverage_closeout_uses_the_sequential_post_trade_curve_and_fee_state() {
    let mut market = concentrated_market();
    market.amm.retain_dynamic_surcharge = true;
    let first = market.quote_leverage_swap(MarketAsset::Base, 50_000, 1).unwrap();
    let first_amm = Market::leverage_amm_quote(first, MarketAsset::Base);
    let expected = market
        .quote_amm_swap_after(&first_amm, MarketAsset::Quote, first.amount_out, 1)
        .unwrap();

    let closeout = market
        .post_swap_closeout_quote_with_quote(MarketAsset::Base, first, MarketAsset::Quote, first.amount_out, 1)
        .unwrap();

    assert_eq!(closeout, expected);
    assert!(expected.fee.volatility_surcharge_debit > 0);
}

#[test]
fn retained_leverage_surcharge_stays_reserve_principal() {
    let mut market = concentrated_market();
    market.amm.retain_dynamic_surcharge = true;
    let quote = market.quote_leverage_swap(MarketAsset::Base, 50_000, 1).unwrap();
    let fee_credit = full_fee_credit(&quote);
    let input_live_before = market.base_side.reserves.live_reserve;
    let protected_before = market.amm.spendable_protected_profit_nad();
    let fee_eligible_ylp_supply = market.base_side.shares.ylp_supply;

    market
        .apply_leverage_swap(
            MarketAsset::Base,
            quote,
            quote.amount_out,
            0,
            fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            fee_eligible_ylp_supply,
            1,
        )
        .unwrap();

    assert_eq!(
        market.base_side.reserves.live_reserve - input_live_before,
        quote.reserve_input_credit
    );
    assert_eq!(
        market.base_side.fees.swap_fee_custody_balance,
        quote.fee_breakdown.base_fee_debit
    );
    assert_eq!(quote.fee_breakdown.distributed_surcharge_debit, 0);
    assert!(market.amm.spendable_protected_profit_nad() > protected_before);
}

#[test]
fn distributed_leverage_surcharge_is_all_lp_owned() {
    let mut market = concentrated_market();
    market.amm.retain_dynamic_surcharge = false;
    let quote = market.quote_leverage_swap(MarketAsset::Base, 50_000, 1).unwrap();
    let fee_credit = full_fee_credit(&quote);
    let fee_eligible_ylp_supply = market.base_side.shares.ylp_supply;

    market
        .apply_leverage_swap(
            MarketAsset::Base,
            quote,
            quote.amount_out,
            0,
            fee_credit,
            0,
            ProtocolAuctionSplit::default(),
            fee_eligible_ylp_supply,
            1,
        )
        .unwrap();

    assert!(quote.fee_breakdown.distributed_surcharge_debit > 0);
    assert_eq!(
        market.base_side.fees.swap_fee_custody_balance,
        quote.fee_breakdown.claimable_fee_debit
    );
    assert_eq!(
        market.base_side.fees.swap_fee_liability + market.base_side.fees.unallocated_swap_fee_liability,
        quote.fee_breakdown.claimable_fee_debit
    );
}

#[test]
fn isolated_principal_writeoff_consumes_protected_liquidity() {
    let mut market = concentrated_market();
    market.amm.retain_dynamic_surcharge = true;
    let funding_quote = market.quote_leverage_swap(MarketAsset::Base, 50_000, 1).unwrap();
    let fee_eligible_ylp_supply = market.base_side.shares.ylp_supply;
    market
        .apply_leverage_swap(
            MarketAsset::Base,
            funding_quote,
            funding_quote.amount_out,
            0,
            full_fee_credit(&funding_quote),
            0,
            ProtocolAuctionSplit::default(),
            fee_eligible_ylp_supply,
            1,
        )
        .unwrap();
    let protected_before = market.amm.spendable_protected_profit_nad();
    assert!(protected_before > 0);

    let mut position = seeded_position(&mut market, MarketAsset::Base, 100_000, 10_000);
    let liquidation_quote = market
        .quote_leverage_swap(MarketAsset::Quote, position.collateral_amount, 2)
        .unwrap();
    let receipt = market
        .liquidate_leverage(
            &mut position,
            prepared_leverage_swap(&market, liquidation_quote),
            full_fee_credit(&liquidation_quote),
            0,
            ProtocolAuctionSplit::default(),
            2,
        )
        .unwrap();

    assert!(receipt.principal_written_off > 0);
    assert!(market.amm.spendable_protected_profit_nad() < protected_before);
}

#[test]
fn leverage_operation_advances_the_controller_before_freezing_its_quote() {
    let mut market = concentrated_market();
    market.config.amm.adjustment_threshold_nad = NAD / 100;
    market.config.amm.adjustment_step_nad = NAD / 100;
    market.config.amm.min_adjustment_interval_slots = 1;
    market.amm.price_ema_nad = 2 * NAD;
    market.amm.last_observation_slot = 2;
    market.amm.last_adjustment_slot = 1;
    market.amm.protected_floor_per_share_nad = 0;

    let center_before = market.amm.center_price_nad;
    market.prepare_amm_for_swap(2).unwrap();
    assert!(market.advance_one_amm_controller_target(2).unwrap());
    assert!(market.amm.center_price_nad > center_before);

    let admitted_center = market.amm.center_price_nad;
    let probe_price = market.curve_marginal_price_nad(2).unwrap();
    market.finalize_amm_trade(probe_price, probe_price, 2).unwrap();

    // The lazy controller runs before the leverage quote. Finalization cannot
    // move the curve again inside the same operation.
    assert_eq!(market.amm.center_price_nad, admitted_center);
    assert!(market.amm.retention_target_stale);
}

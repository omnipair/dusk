use super::*;
use crate::{
    constants::{
        BPS_DENOMINATOR, LIQUIDATION_AUCTION_DURATION_SECONDS, LIQUIDATION_BACKSTOP_CALLER_BPS,
        MARKET_LAYOUT_VERSION, NAD,
    },
    state::{Debt, HlpVault, Insurance, MarketConfig, MarketSide, Reserves, Risk},
};
use proptest::prelude::*;

fn valid_config() -> MarketConfig {
    MarketConfig {
        swap_fee_bps: 30,
        divergence_fee_share_cap_bps: 0,
        volatility_fee_share_cap_bps: 0,
        target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
        settlement_divergence_bps: 500,
        ema_half_life_ms: 60_000,
        directional_ema_half_life_ms: 60_000,
        curve_depth_ema_half_life_ms: 60_000,
        max_daily_borrow_bps: 2_000,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 11_000,
        amm: Default::default(),
        irm: Default::default(),
        start_time: 0,
    }
}

fn liquidatable_quote_debt_position() -> (Market, BorrowPosition) {
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let mut base_side = MarketSide {
        asset_mint: base_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    base_side.reserves = Reserves {
        live_reserve: 1_000_000_000,
        cash_reserve: 1_000_000_000,
        ..Reserves::default()
    };
    let mut quote_side = MarketSide {
        asset_mint: quote_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    quote_side.reserves = Reserves {
        live_reserve: 1_000_000_000,
        cash_reserve: 1_000_000_000,
        ..Reserves::default()
    };

    let debt = Debt {
        fixed_quote_shares: 100,
        quote_borrow_index_nad: NAD as u128,
        base_borrow_index_nad: NAD as u128,
        fixed_quote_principal: 100,
        global_health_base_contribution_for_quote_debt: 109,
        ..Debt::default()
    };
    base_side.shares.ylp_supply = 1_000_000_000;
    quote_side.shares.ylp_supply = 1_000_000_000;
    let mut market = Market {
        version: MARKET_LAYOUT_VERSION,
        ylp_mint: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: valid_config(),
        amm: Default::default(),
        debt,
        base_hlp_vault: HlpVault::default(),
        quote_hlp_vault: HlpVault::default(),
        risk: Risk::default(),
        insurance: Insurance::default(),
        params_hash: [9; 32],
        initial_liquidity_authority: Pubkey::default(),
        governance_locked_ylp: 0,
        parameter_revisions: [0; 7],
        last_marginal_observation_nad: 0,
        curve_revision: 0,
        risk_revision: 0,
        last_update_slot: 0,
        reduce_only: false,
        bump: 255,
    };
    market.prepare_amm_for_swap(0).unwrap();
    market.refresh_risk().unwrap();
    let borrow_position = BorrowPosition {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        base_collateral: 109,
        quote_collateral: 0,
        global_health_base_contribution_for_quote_debt: 109,
        global_health_quote_contribution_for_base_debt: 0,
        base_liquidation_cf_bps: 0,
        quote_liquidation_cf_bps: 8_500,
        base_referral_partner: Pubkey::default(),
        quote_referral_partner: Pubkey::default(),
        base_referral_interest_share_bps: 0,
        quote_referral_interest_share_bps: 0,
        fixed_base_shares: 0,
        fixed_quote_shares: 100,
        auction_debt_asset: u8::MAX,
        auction_start_time: 0,
        auction_start_price_nad: 0,
        auction_floor_price_nad: 0,
        bump: 255,
    };
    (market, borrow_position)
}

fn market_with_cash_backed_debt(
    debt_asset: MarketAsset,
    debt_cash: u64,
    collateral_cash: u64,
    borrow_amount: u64,
    interest_bps: u128,
) -> (Market, BorrowPosition) {
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let next_index = (NAD as u128)
        .checked_mul((BPS_DENOMINATOR as u128).checked_add(interest_bps).unwrap())
        .unwrap()
        .checked_div(BPS_DENOMINATOR as u128)
        .unwrap();
    let shares = Debt::debt_to_shares(borrow_amount, NAD as u128).unwrap();
    let current_debt = Debt::shares_to_debt(shares, next_index).unwrap();
    let debt_cash_after_borrow = debt_cash.checked_sub(borrow_amount).unwrap();
    let debt_live = debt_cash_after_borrow
        .checked_add(u64::try_from(current_debt).unwrap())
        .unwrap();

    let mut base_side = MarketSide {
        asset_mint: base_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    let mut quote_side = MarketSide {
        asset_mint: quote_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    let mut debt = Debt {
        base_borrow_index_nad: NAD as u128,
        quote_borrow_index_nad: NAD as u128,
        ..Debt::default()
    };
    let collateral_amount = u64::try_from(current_debt).unwrap().checked_mul(2).unwrap();
    let mut borrow_position = BorrowPosition {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        base_collateral: 0,
        quote_collateral: 0,
        global_health_base_contribution_for_quote_debt: 0,
        global_health_quote_contribution_for_base_debt: 0,
        base_liquidation_cf_bps: 8_500,
        quote_liquidation_cf_bps: 8_500,
        base_referral_partner: Pubkey::default(),
        quote_referral_partner: Pubkey::default(),
        base_referral_interest_share_bps: 0,
        quote_referral_interest_share_bps: 0,
        fixed_base_shares: 0,
        fixed_quote_shares: 0,
        auction_debt_asset: u8::MAX,
        auction_start_time: 0,
        auction_start_price_nad: 0,
        auction_floor_price_nad: 0,
        bump: 255,
    };

    match debt_asset {
        MarketAsset::Base => {
            base_side.reserves = Reserves {
                live_reserve: debt_live,
                cash_reserve: debt_cash_after_borrow,
                ..Reserves::default()
            };
            quote_side.reserves = Reserves {
                live_reserve: collateral_cash,
                cash_reserve: collateral_cash,
                ..Reserves::default()
            };
            debt.base_borrow_index_nad = next_index;
            debt.fixed_base_shares = shares;
            debt.fixed_base_principal = borrow_amount;
            debt.global_health_quote_contribution_for_base_debt = collateral_amount;
            borrow_position.fixed_base_shares = shares;
            borrow_position.quote_collateral = collateral_amount;
            borrow_position.global_health_quote_contribution_for_base_debt = collateral_amount;
        }
        MarketAsset::Quote => {
            base_side.reserves = Reserves {
                live_reserve: collateral_cash,
                cash_reserve: collateral_cash,
                ..Reserves::default()
            };
            quote_side.reserves = Reserves {
                live_reserve: debt_live,
                cash_reserve: debt_cash_after_borrow,
                ..Reserves::default()
            };
            debt.quote_borrow_index_nad = next_index;
            debt.fixed_quote_shares = shares;
            debt.fixed_quote_principal = borrow_amount;
            debt.global_health_base_contribution_for_quote_debt = collateral_amount;
            borrow_position.fixed_quote_shares = shares;
            borrow_position.base_collateral = collateral_amount;
            borrow_position.global_health_base_contribution_for_quote_debt = collateral_amount;
        }
    }

    let ylp_supply = debt_live.min(collateral_cash).max(1);
    base_side.shares.ylp_supply = ylp_supply;
    quote_side.shares.ylp_supply = ylp_supply;
    let mut market = Market {
        version: MARKET_LAYOUT_VERSION,
        ylp_mint: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: valid_config(),
        amm: Default::default(),
        debt,
        base_hlp_vault: HlpVault::default(),
        quote_hlp_vault: HlpVault::default(),
        risk: Risk::default(),
        insurance: Insurance::default(),
        params_hash: [7; 32],
        initial_liquidity_authority: Pubkey::default(),
        governance_locked_ylp: 0,
        parameter_revisions: [0; 7],
        last_marginal_observation_nad: 0,
        curve_revision: 0,
        risk_revision: 0,
        last_update_slot: 0,
        reduce_only: false,
        bump: 255,
    };
    market.prepare_amm_for_swap(0).unwrap();
    market.refresh_risk().unwrap();

    (market, borrow_position)
}

fn liquidation_terms_for_debt(debt: u128) -> LiquidationTerms {
    LiquidationTerms {
        liquidation_incentive_bps: 0,
        insurance_funding_bps: 0,
        total_penalty_bps: 0,
        max_repay_amount: u64::try_from(debt).unwrap(),
    }
}

fn position_debt_after(market: &Market, borrow_position: &BorrowPosition, debt_asset: MarketAsset) -> u128 {
    match debt_asset {
        MarketAsset::Base => borrow_position.fixed_base_debt(&market.debt).unwrap(),
        MarketAsset::Quote => borrow_position.fixed_quote_debt(&market.debt).unwrap(),
    }
}

fn reserve_pair(market: &Market, asset: MarketAsset) -> (u64, u64) {
    let side = market.side(asset);
    (side.reserves.live_reserve, side.reserves.cash_reserve)
}

fn expected_insurance_funding_bps(liquidation_incentive_bps: u16, liquidation_health_floor_bps: u64) -> u16 {
    let max_total_penalty = liquidation_health_floor_bps.saturating_sub(BPS_DENOMINATOR as u64 + 1);
    let remaining_penalty_room = max_total_penalty.saturating_sub(liquidation_incentive_bps as u64);
    LIQUIDATION_INSURANCE_FUNDING_BPS.min(u16::try_from(remaining_penalty_room).unwrap_or(u16::MAX))
}

#[test]
fn euler_style_incentive_grows_with_health_shortfall() {
    assert_eq!(liquidation_max_incentive_bps(10_999, 11_000), 100);
    assert_eq!(liquidation_max_incentive_bps(10_750, 11_000), 250);
    assert_eq!(liquidation_max_incentive_bps(9_000, 11_000), 500);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn liquidation_preserves_cash_backed_virtual_reserve_invariant_under_rounded_debt_burns(
        liquidate_base in any::<bool>(),
        debt_cash in 1_000_000u64..50_000_000,
        collateral_cash in 1_000_000u64..50_000_000,
        borrow_bps in 1u64..=500,
        interest_bps in 1u128..=2_000,
        repay_bps in 1u128..=5_000,
    ) {
        let debt_asset = if liquidate_base {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let borrow_amount = debt_cash
            .checked_mul(borrow_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u64)
            .unwrap()
            .max(1);
        let (mut market, mut borrow_position) = market_with_cash_backed_debt(
            debt_asset,
            debt_cash,
            collateral_cash,
            borrow_amount,
            interest_bps,
        );
        let debt_before = position_debt_after(&market, &borrow_position, debt_asset);
        let repay_credit = debt_before
            .checked_mul(repay_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u128)
            .unwrap()
            .max(1)
            .min(debt_before);
        let max_repay_credit = u64::try_from(repay_credit).unwrap();
        let repay_credit = market
            .fixed_repayment_for_max(&borrow_position, debt_asset, max_repay_credit)
            .unwrap()
            .cash_repaid;
        let aggregate_debt_before = match debt_asset {
            MarketAsset::Base => market.debt.fixed_base_debt().unwrap(),
            MarketAsset::Quote => market.debt.fixed_quote_debt().unwrap(),
        };
        let (live_before, cash_before) = reserve_pair(&market, debt_asset);
        let pricing = LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad: NAD,
        };

        let receipt = Liquidation::new_with_pricing(
            debt_asset,
            repay_credit,
            0,
            0,
            0,
            liquidation_terms_for_debt(debt_before),
            pricing,
        )
        .apply(&mut market, &mut borrow_position)
        .unwrap();

        let debt_after = position_debt_after(&market, &borrow_position, debt_asset);
        let debt_reduction = debt_before.checked_sub(debt_after).unwrap();
        let debt_reduction = u64::try_from(debt_reduction).unwrap();
        let aggregate_debt_after = match debt_asset {
            MarketAsset::Base => market.debt.fixed_base_debt().unwrap(),
            MarketAsset::Quote => market.debt.fixed_quote_debt().unwrap(),
        };
        let aggregate_debt_reduction = u64::try_from(aggregate_debt_before - aggregate_debt_after).unwrap();
        let principal_credit = repay_credit.checked_sub(receipt.interest_paid).unwrap();
        let live_debit = aggregate_debt_reduction.checked_sub(principal_credit).unwrap();
        let (live_after, cash_after) = reserve_pair(&market, debt_asset);

        prop_assert_eq!(receipt.socialized_loss, 0);
        prop_assert_eq!(receipt.insurance_drawn, 0);
        prop_assert_eq!(live_after, live_before - live_debit);
        prop_assert_eq!(cash_after, cash_before + principal_credit);
        prop_assert!(repay_credit <= max_repay_credit);
        prop_assert!(debt_reduction.abs_diff(repay_credit) <= 1);
        prop_assert_eq!(aggregate_debt_reduction, repay_credit);
        market.assert_market_invariants().unwrap();
    }
}

#[test]
fn partial_liquidation_charges_the_aggregate_delta_without_rounding_writeoff() {
    let debt_asset = MarketAsset::Quote;
    let debt_cash = 28_642_837;
    let borrow_amount = debt_cash * 346 / BPS_DENOMINATOR as u64;
    let (mut market, mut borrow_position) =
        market_with_cash_backed_debt(debt_asset, debt_cash, 1_000_000, borrow_amount, 413);
    let debt_before = position_debt_after(&market, &borrow_position, debt_asset);
    let max_repay_credit = u64::try_from(debt_before * 205 / BPS_DENOMINATOR as u128).unwrap();
    let repay_credit = market
        .fixed_repayment_for_max(&borrow_position, debt_asset, max_repay_credit)
        .unwrap()
        .cash_repaid;
    let aggregate_debt_before = market.debt.fixed_quote_debt().unwrap();
    let (live_before, cash_before) = reserve_pair(&market, debt_asset);
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD,
    };

    let receipt = Liquidation::new_with_pricing(
        debt_asset,
        repay_credit,
        0,
        0,
        0,
        liquidation_terms_for_debt(debt_before),
        pricing,
    )
    .apply(&mut market, &mut borrow_position)
    .unwrap();

    let debt_after = position_debt_after(&market, &borrow_position, debt_asset);
    let debt_reduction = u64::try_from(debt_before - debt_after).unwrap();
    let aggregate_debt_after = market.debt.fixed_quote_debt().unwrap();
    let aggregate_debt_reduction = u64::try_from(aggregate_debt_before - aggregate_debt_after).unwrap();
    let principal_credit = repay_credit.checked_sub(receipt.interest_paid).unwrap();
    assert!(debt_reduction.abs_diff(repay_credit) <= 1);
    assert_eq!(aggregate_debt_reduction, repay_credit);
    assert_eq!(
        market.quote_side.reserves.live_reserve,
        live_before - (aggregate_debt_reduction - principal_credit)
    );
    assert_eq!(market.quote_side.reserves.cash_reserve, cash_before + principal_credit);
    market.assert_market_invariants().unwrap();
}

#[test]
fn partial_liquidation_uses_aggregate_debt_delta_with_multiple_positions() {
    let debt_asset = MarketAsset::Quote;
    let borrow_amount = 50_000_003;
    let (mut market, mut borrow_position) =
        market_with_cash_backed_debt(debt_asset, 200_000_000, 300_000_000, borrow_amount, 413);
    let second_position_shares = borrow_position.fixed_quote_shares;
    market.debt.fixed_quote_shares += second_position_shares;
    market.debt.fixed_quote_principal += borrow_amount;
    market.debt.global_health_base_contribution_for_quote_debt +=
        borrow_position.global_health_base_contribution_for_quote_debt;
    market.quote_side.reserves.cash_reserve -= borrow_amount;
    market.quote_side.reserves.live_reserve = u64::try_from(
        market.quote_side.reserves.cash_reserve as u128
            + Debt::shares_to_debt(market.debt.fixed_quote_shares, market.debt.quote_borrow_index_nad).unwrap(),
    )
    .unwrap();
    market.assert_virtual_reserve_invariant(debt_asset).unwrap();
    let debt_before = position_debt_after(&market, &borrow_position, debt_asset);

    let repay_credit = market
        .fixed_repayment_for_max(&borrow_position, debt_asset, 25_000_004)
        .unwrap()
        .cash_repaid;
    Liquidation::new_with_pricing(
        debt_asset,
        repay_credit,
        0,
        0,
        0,
        liquidation_terms_for_debt(debt_before),
        LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad: NAD,
        },
    )
    .apply(&mut market, &mut borrow_position)
    .unwrap();

    market.assert_virtual_reserve_invariant(debt_asset).unwrap();
}

#[test]
fn partial_liquidation_recalculates_contribution_and_stored_cf() {
    let debt_asset = MarketAsset::Quote;
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    market.quote_side.reserves.live_reserve += 100;
    borrow_position.quote_liquidation_cf_bps = 4_000;
    let old_contribution = borrow_position.global_health_base_contribution_for_quote_debt;
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD,
    };

    let receipt = Liquidation::new_with_pricing(debt_asset, 20, 0, 0, 0, liquidation_terms_for_debt(100), pricing)
        .apply(&mut market, &mut borrow_position)
        .unwrap();

    assert_eq!(receipt.remaining_debt, 80);
    assert_eq!(borrow_position.quote_liquidation_cf_bps, 8_500);
    assert!(borrow_position.global_health_base_contribution_for_quote_debt < old_contribution);
    assert_eq!(
        market.debt.global_health_base_contribution_for_quote_debt,
        borrow_position.global_health_base_contribution_for_quote_debt
    );
}

#[test]
fn insurance_credit_liquidation_closes_debt_without_breaking_virtual_reserve_invariant() {
    let debt_asset = MarketAsset::Quote;
    let (mut market, mut borrow_position) =
        market_with_cash_backed_debt(debt_asset, 2_000_000, 2_000_000, 100_000, 500);
    let debt_before = position_debt_after(&market, &borrow_position, debt_asset);
    let debt_before_u64 = u64::try_from(debt_before).unwrap();
    let repay_credit = debt_before_u64 / 2;
    let insurance_credit = debt_before_u64 - repay_credit;
    // A single liquidation may use at most 20% of available insurance.
    market.insurance.quote_available = insurance_credit * 5;
    let (live_before, cash_before) = reserve_pair(&market, debt_asset);
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD,
    };

    let receipt = Liquidation::new_with_pricing(
        debt_asset,
        repay_credit,
        insurance_credit,
        insurance_credit,
        0,
        liquidation_terms_for_debt(debt_before),
        pricing,
    )
    .apply(&mut market, &mut borrow_position)
    .unwrap();

    let principal_credit = (repay_credit + insurance_credit)
        .checked_sub(receipt.interest_paid)
        .unwrap();
    assert_eq!(receipt.insurance_drawn, insurance_credit);
    assert_eq!(receipt.socialized_loss, 0);
    assert_eq!(receipt.remaining_debt, 0);
    assert_eq!(
        market.quote_side.reserves.live_reserve,
        live_before - (debt_before_u64 - principal_credit)
    );
    assert_eq!(market.quote_side.reserves.cash_reserve, cash_before + principal_credit);
    market.assert_market_invariants().unwrap();
}

#[test]
fn collateral_exhausted_liquidation_socializes_loss_without_breaking_virtual_reserve_invariant() {
    let debt_asset = MarketAsset::Quote;
    let (mut market, mut borrow_position) =
        market_with_cash_backed_debt(debt_asset, 2_000_000, 2_000_000, 100_000, 500);
    borrow_position.base_collateral = 1;
    borrow_position.global_health_base_contribution_for_quote_debt = 1;
    market.debt.global_health_base_contribution_for_quote_debt = 1;
    let debt_before = position_debt_after(&market, &borrow_position, debt_asset);
    let debt_before_u64 = u64::try_from(debt_before).unwrap();
    let repay_credit = debt_before_u64 / 2;
    let max_socialized_loss = debt_before_u64 - repay_credit;
    let (live_before, cash_before) = reserve_pair(&market, debt_asset);
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD,
    };

    let receipt = Liquidation::new_with_pricing(
        debt_asset,
        repay_credit,
        0,
        0,
        max_socialized_loss,
        liquidation_terms_for_debt(debt_before),
        pricing,
    )
    .apply(&mut market, &mut borrow_position)
    .unwrap();

    let principal_credit = repay_credit.checked_sub(receipt.interest_paid).unwrap();
    assert_eq!(receipt.collateral_seized, 1);
    assert_eq!(receipt.socialized_loss, max_socialized_loss);
    assert_eq!(receipt.remaining_debt, 0);
    assert_eq!(
        market.quote_side.reserves.live_reserve,
        live_before - (debt_before_u64 - principal_credit)
    );
    assert_eq!(market.quote_side.reserves.cash_reserve, cash_before + principal_credit);
    market.assert_market_invariants().unwrap();
}

#[test]
fn internal_floor_uses_insurance_then_socializes_and_closes_without_caller_cap() {
    let debt_asset = MarketAsset::Quote;
    let (mut market, mut borrow_position) =
        market_with_cash_backed_debt(debt_asset, 2_000_000, 2_000_000, 100_000, 500);
    let full_repayment = market
        .fixed_repayment_for_max(&borrow_position, debt_asset, u64::MAX)
        .unwrap()
        .cash_repaid;
    let collateral_consumed = borrow_position.base_collateral;
    let caller_bounty = u64::try_from(
        (collateral_consumed as u128) * LIQUIDATION_BACKSTOP_CALLER_BPS as u128
            / BPS_DENOMINATOR as u128,
    )
    .unwrap();
    let swap_output = full_repayment / 2;
    let insurance_credit = full_repayment / 10;
    market.insurance.quote_available = insurance_credit * 5;
    borrow_position.start_liquidation_auction(MarketAsset::Quote, 1, NAD, NAD);

    let receipt = market
        .settle_internal_liquidation(
            &mut borrow_position,
            debt_asset,
            swap_output,
            insurance_credit,
            insurance_credit,
            collateral_consumed,
            caller_bounty,
        )
        .unwrap();

    assert_eq!(receipt.liquidation.repaid_amount, swap_output);
    assert_eq!(receipt.liquidation.insurance_drawn, insurance_credit);
    assert_eq!(
        receipt.liquidation.socialized_loss,
        full_repayment - swap_output - insurance_credit
    );
    assert_eq!(receipt.liquidation.collateral_to_liquidator, caller_bounty);
    assert_eq!(receipt.owner_residual, 0);
    assert_eq!(borrow_position.base_collateral, 0);
    assert_eq!(borrow_position.fixed_quote_shares, 0);
    assert_eq!(market.debt.fixed_quote_shares, 0);
    assert_eq!(borrow_position.quote_liquidation_cf_bps, 0);
    assert_eq!(borrow_position.global_health_base_contribution_for_quote_debt, 0);
    assert!(!borrow_position.has_active_liquidation_auction());
}

#[test]
fn internal_floor_returns_solvent_swap_residual_to_owner() {
    let debt_asset = MarketAsset::Quote;
    let (mut market, mut borrow_position) =
        market_with_cash_backed_debt(debt_asset, 2_000_000, 2_000_000, 100_000, 0);
    let full_repayment = market
        .fixed_repayment_for_max(&borrow_position, debt_asset, u64::MAX)
        .unwrap()
        .cash_repaid;
    let collateral_consumed = borrow_position.base_collateral;

    let receipt = market
        .settle_internal_liquidation(
            &mut borrow_position,
            debt_asset,
            full_repayment + 123,
            0,
            0,
            collateral_consumed,
            0,
        )
        .unwrap();

    assert_eq!(receipt.owner_residual, 123);
    assert_eq!(receipt.liquidation.repaid_amount, full_repayment);
    assert_eq!(receipt.liquidation.insurance_drawn, 0);
    assert_eq!(receipt.liquidation.socialized_loss, 0);
    assert_eq!(receipt.liquidation.remaining_debt, 0);
}

#[test]
fn insurance_funding_preserves_room_to_restore_health() {
    assert_eq!(expected_insurance_funding_bps(100, 11_000), 200);
    assert_eq!(expected_insurance_funding_bps(500, 11_000), 200);
    assert_eq!(expected_insurance_funding_bps(200, 10_250), 49);
}

#[test]
fn liquidation_eligibility_is_inclusive_at_stored_cf_equality() {
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    borrow_position.base_collateral = 1_000_000_000;
    borrow_position.quote_liquidation_cf_bps = 8_500;
    let risk = market.current_risk().unwrap();
    let collateral_value_nad = market
        .linear_liquidation_collateral_value_nad(MarketAsset::Base, borrow_position.base_collateral, &risk)
        .unwrap();
    let threshold_debt_nad = crate::math::ceil_div(
        collateral_value_nad * borrow_position.quote_liquidation_cf_bps as u128,
        BPS_DENOMINATOR as u128,
    )
    .unwrap();
    let threshold_debt = crate::math::ceil_div(threshold_debt_nad, NAD as u128).unwrap();
    market.debt.fixed_quote_shares = threshold_debt;
    market.debt.fixed_quote_principal = u64::try_from(threshold_debt).unwrap();
    borrow_position.fixed_quote_shares = threshold_debt;

    assert!(market
        .is_position_liquidatable_with_risk(&borrow_position, MarketAsset::Quote, &risk)
        .unwrap());

    market.debt.fixed_quote_shares -= 1;
    borrow_position.fixed_quote_shares -= 1;
    assert!(!market
        .is_position_liquidatable_with_risk(&borrow_position, MarketAsset::Quote, &risk)
        .unwrap());
}

#[test]
fn liquidation_threshold_is_linear_and_independent_of_curve_depth() {
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    borrow_position.base_collateral = 1_000_000_000;
    market.debt.fixed_quote_shares = 600_000_000;
    market.debt.fixed_quote_principal = 600_000_000;
    borrow_position.fixed_quote_shares = 600_000_000;

    let deep_risk = market.current_risk().unwrap();
    let shallow_risk = Risk {
        directional_base_price_ema_nad: NAD / 10,
        observed_curve_depth_nad: deep_risk.observed_curve_depth_nad / 100,
        curve_depth_ema_nad: deep_risk.curve_depth_ema_nad / 100,
        ..deep_risk
    };
    let unwind_value_nad = market
        .liquidation_collateral_value_nad(MarketAsset::Base, borrow_position.base_collateral, &deep_risk)
        .unwrap();

    // A slippage-adjusted trigger would liquidate this position early.
    assert!(600_000_000_u128 * NAD as u128 * BPS_DENOMINATOR as u128
        >= unwind_value_nad * borrow_position.quote_liquidation_cf_bps as u128);
    assert_eq!(
        market
            .linear_liquidation_collateral_value_nad(MarketAsset::Base, borrow_position.base_collateral, &deep_risk)
            .unwrap(),
        market
            .linear_liquidation_collateral_value_nad(MarketAsset::Base, borrow_position.base_collateral, &shallow_risk)
            .unwrap()
    );
    assert!(!market
        .is_position_liquidatable_with_risk(&borrow_position, MarketAsset::Quote, &deep_risk)
        .unwrap());
    assert!(!market
        .is_position_liquidatable_with_risk(&borrow_position, MarketAsset::Quote, &shallow_risk)
        .unwrap());
}

#[test]
fn auction_floor_scales_concentrated_and_tail_depth_together() {
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    market.config.amm.peak_amplification_nad = 4 * NAD;
    market.config.amm.core_half_width_bps = 1_000;
    market.config.amm.fade_width_bps = 1_000;
    market.amm = Default::default();
    market.risk = Risk::default();
    market.prepare_amm_for_swap(0).unwrap();
    market.refresh_risk().unwrap();
    borrow_position.base_collateral = 100_000_000;

    let cache = market.amm.explicit_curve_cache;
    let current_depth = cache.tail_liquidity + cache.concentrated_liquidity;
    let target_depth = current_depth / 2;
    let shallow_risk = Risk {
        base_price_ema_nad: NAD,
        quote_price_ema_nad: NAD,
        directional_base_price_ema_nad: NAD,
        directional_quote_price_ema_nad: NAD,
        observed_curve_depth_nad: target_depth,
        curve_depth_ema_nad: target_depth,
        ..Risk::default()
    };
    let actual = market
        .liquidation_collateral_value_nad(MarketAsset::Base, borrow_position.base_collateral, &shallow_risk)
        .unwrap();

    let scaled_tail = crate::math::mul_div_u128(cache.tail_liquidity, target_depth, current_depth).unwrap();
    let scaled_concentrated =
        crate::math::mul_div_u128(cache.concentrated_liquidity, target_depth, current_depth).unwrap();
    let mut scaled_cache = cache;
    scaled_cache.tail_liquidity = scaled_tail;
    scaled_cache.concentrated_liquidity = scaled_concentrated;
    let scaled_geometry = scaled_cache.geometry().unwrap();
    let scaled_point = scaled_geometry.point_at_price_nad(NAD as u128, scaled_tail).unwrap();
    let expected = scaled_geometry
        .quote_exact_in(
            scaled_point,
            borrow_position.base_collateral as u128 * NAD as u128,
            ExplicitCurveDirection::BaseToQuote,
        )
        .unwrap()
        .amount_out;

    // Regression guard: scaling only the tail leaves the old concentrated
    // tranche in place instead of rebuilding the complete depth-capped curve.
    let old_geometry = cache.geometry().unwrap();
    let old_point = old_geometry.point_at_price_nad(NAD as u128, scaled_tail).unwrap();
    let tail_only_scaled = old_geometry
        .quote_exact_in(
            old_point,
            borrow_position.base_collateral as u128 * NAD as u128,
            ExplicitCurveDirection::BaseToQuote,
        )
        .unwrap()
        .amount_out;

    assert_eq!(actual, expected);
    assert_ne!(actual, tail_only_scaled);
}

#[test]
fn auction_floor_uses_pessimistic_average_unwind_price() {
    let (market, borrow_position) = liquidatable_quote_debt_position();
    let risk = market.current_risk().unwrap();
    let collateral_value_nad = market
        .liquidation_collateral_value_nad(MarketAsset::Base, borrow_position.base_collateral, &risk)
        .unwrap();
    let floor_price_nad = market
        .liquidation_reference_price_nad(&borrow_position, MarketAsset::Quote)
        .unwrap();
    let reference_value_nad = (borrow_position.base_collateral as u128)
        .checked_mul(floor_price_nad as u128)
        .unwrap();

    assert!(floor_price_nad < NAD);
    assert!(reference_value_nad <= collateral_value_nad);
    assert!(collateral_value_nad - reference_value_nad <= borrow_position.base_collateral as u128);
}

#[test]
fn liquidation_auction_is_bound_to_one_debt_asset() {
    let (_, mut borrow_position) = liquidatable_quote_debt_position();
    borrow_position.start_liquidation_auction(MarketAsset::Quote, 1, NAD, NAD);

    borrow_position.assert_liquidation_auction(MarketAsset::Quote).unwrap();
    assert_eq!(
        borrow_position
            .assert_liquidation_auction(MarketAsset::Base)
            .unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::PositionNotLiquidatable)
    );

    borrow_position.clear_liquidation_auction();
    assert!(!borrow_position.has_active_liquidation_auction());
}

#[test]
fn liquidation_auction_reaches_floor_only_at_explicit_expiry() {
    let (_, mut borrow_position) = liquidatable_quote_debt_position();
    let start_time = 10;
    borrow_position.start_liquidation_auction(MarketAsset::Quote, start_time, 105, 100);

    assert_eq!(borrow_position.liquidation_auction_price_nad(start_time).unwrap(), 105);
    assert_eq!(
        borrow_position
            .liquidation_auction_price_nad(start_time + LIQUIDATION_AUCTION_DURATION_SECONDS - 1)
            .unwrap(),
        101
    );
    assert!(!borrow_position
        .liquidation_auction_expired(start_time + LIQUIDATION_AUCTION_DURATION_SECONDS - 1)
        .unwrap());
    assert_eq!(
        borrow_position
            .liquidation_auction_price_nad(start_time + LIQUIDATION_AUCTION_DURATION_SECONDS)
            .unwrap(),
        100
    );
    assert!(borrow_position
        .liquidation_auction_expired(start_time + LIQUIDATION_AUCTION_DURATION_SECONDS)
        .unwrap());
}

#[test]
fn recovered_position_cancels_active_auction_before_settlement() {
    let (market, mut borrow_position) = liquidatable_quote_debt_position();
    borrow_position.start_liquidation_auction(MarketAsset::Quote, 1, NAD, NAD);
    assert!(market
        .is_position_liquidatable(&borrow_position, MarketAsset::Quote)
        .unwrap());

    borrow_position.base_collateral = 1_000;
    assert!(!market
        .is_position_liquidatable(&borrow_position, MarketAsset::Quote)
        .unwrap());
    market.reconcile_liquidation_auction(&mut borrow_position).unwrap();

    assert!(!borrow_position.has_active_liquidation_auction());
    assert_eq!(
        borrow_position
            .assert_liquidation_auction(MarketAsset::Quote)
            .unwrap_err(),
        error!(ErrorCode::PositionNotLiquidatable)
    );
}

#[test]
fn max_repay_caps_liquidation_to_restore_target_health() {
    let (market, borrow_position) = liquidatable_quote_debt_position();
    let target_health_bps = liquidation_health_floor_bps(borrow_position.quote_liquidation_cf_bps);
    let incentive_bps = liquidation_max_incentive_bps(10_900, target_health_bps);
    let insurance_bps = expected_insurance_funding_bps(incentive_bps, target_health_bps);
    let cap = max_repay_to_restore_health_with_pricing(
        &market,
        &borrow_position,
        MarketAsset::Quote,
        incentive_bps + insurance_bps,
        LiquidationPricing::PessimisticReserves,
    )
    .unwrap();

    assert_eq!(cap, 82);
}

#[test]
fn reference_pricing_uses_ema_price_for_collateral_seizure() {
    let (market, _) = liquidatable_quote_debt_position();
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD,
    };

    let seized = collateral_amount_for_debt_value_with_pricing(&market, MarketAsset::Quote, 100, 300, pricing).unwrap();
    let bidder_collateral =
        collateral_amount_for_debt_value_with_pricing(&market, MarketAsset::Quote, 100, 100, pricing).unwrap();

    assert_eq!(seized, 103);
    assert_eq!(bidder_collateral, 101);
}

#[test]
fn direct_liquidation_restore_cap_uses_reference_price() {
    let (market, borrow_position) = liquidatable_quote_debt_position();
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD,
    };
    let cap =
        max_repay_to_restore_health_with_pricing(&market, &borrow_position, MarketAsset::Quote, 300, pricing).unwrap();

    assert_eq!(cap, 60);
}

#[test]
fn max_repay_respects_close_factor_for_deep_partial_liquidation() {
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    borrow_position.base_collateral = 50;
    borrow_position.global_health_base_contribution_for_quote_debt = 50;
    market.debt.global_health_base_contribution_for_quote_debt = 50;
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD,
    };
    let terms = market
        .liquidation_terms_with_pricing(&borrow_position, MarketAsset::Quote, pricing)
        .unwrap();

    assert_eq!(terms.max_repay_amount, 50);
}

#[test]
fn max_repay_full_closes_when_partial_would_leave_dust() {
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    market.debt.fixed_quote_shares = 2;
    market.debt.fixed_quote_principal = 2;
    market.debt.global_health_base_contribution_for_quote_debt = 1;
    borrow_position.fixed_quote_shares = 2;
    borrow_position.base_collateral = 1;
    borrow_position.global_health_base_contribution_for_quote_debt = 1;
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD,
    };
    let terms = market
        .liquidation_terms_with_pricing(&borrow_position, MarketAsset::Quote, pricing)
        .unwrap();

    assert_eq!(terms.max_repay_amount, 2);
}

#[test]
fn liquidation_rejects_repay_above_restore_cap() {
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    let target_health_bps = liquidation_health_floor_bps(borrow_position.quote_liquidation_cf_bps);
    let incentive_bps = liquidation_max_incentive_bps(10_900, target_health_bps);
    let insurance_bps = expected_insurance_funding_bps(incentive_bps, target_health_bps);
    let cap = max_repay_to_restore_health_with_pricing(
        &market,
        &borrow_position,
        MarketAsset::Quote,
        incentive_bps + insurance_bps,
        LiquidationPricing::PessimisticReserves,
    )
    .unwrap();

    let terms = market
        .liquidation_terms_with_pricing(
            &borrow_position,
            MarketAsset::Quote,
            LiquidationPricing::PessimisticReserves,
        )
        .unwrap();
    let err = Liquidation::new(MarketAsset::Quote, cap + 1, 0, 0, 0, terms)
        .apply(&mut market, &mut borrow_position)
        .unwrap_err();

    assert_eq!(err, anchor_lang::prelude::error!(ErrorCode::LiquidationRepayTooLarge));
}

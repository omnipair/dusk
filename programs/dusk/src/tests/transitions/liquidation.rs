use super::*;
use crate::{
    constants::{BPS_DENOMINATOR, MARKET_VERSION, NAD},
    state::{
        Debt, HlpVault, Insurance, MarketConfig, MarketSide, PendingAuthorityChange,
        PendingConfigChange, Reserves, Risk,
    },
};
use proptest::prelude::*;

fn valid_config() -> MarketConfig {
    MarketConfig {
        swap_fee_bps: 30,
        manager_fee_bps: 0,
        protocol_fee_bps: 0,
        target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
        settlement_divergence_bps: 500,
        ema_half_life_ms: 60_000,
        directional_ema_half_life_ms: 60_000,
        k_ema_half_life_ms: 60_000,
        max_daily_borrow_bps: 2_000,
        spot_ema_divergence_bps: 1_000,
        k_ema_drawdown_bps: 1_000,
        utilized_collateral_cap_bps: 15_000,
        market_health_min_bps: 11_000,
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
        reserved_liability: 0,
    };
    let mut quote_side = MarketSide {
        asset_mint: quote_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    quote_side.reserves = Reserves {
        live_reserve: 1_000_000_000,
        cash_reserve: 1_000_000_000,
        reserved_liability: 0,
    };

    let debt = Debt {
        fixed_quote_shares: 100,
        quote_borrow_index_nad: NAD as u128,
        base_borrow_index_nad: NAD as u128,
        fixed_quote_principal: 100,
        utilized_base_collateral_for_quote_debt: 109,
        ..Debt::default()
    };
    let market = Market {
        version: MARKET_VERSION,
        ylp_mint: Pubkey::new_unique(),
        operator: Pubkey::new_unique(),
        manager: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: valid_config(),
        debt,
        base_hlp_vault: HlpVault::default(),
        quote_hlp_vault: HlpVault::default(),
        risk: Risk::default(),
        insurance: Insurance::default(),
        pending_config: PendingConfigChange::default(),
        pending_operator: PendingAuthorityChange::default(),
        pending_manager: PendingAuthorityChange::default(),
        params_hash: [9; 32],
        last_update_slot: 0,
        reduce_only: false,
        bump: 255,
    };
    let borrow_position = BorrowPosition {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        base_collateral: 109,
        quote_collateral: 0,
        utilized_base_collateral_for_quote_debt: 109,
        utilized_quote_collateral_for_base_debt: 0,
        fixed_base_shares: 0,
        fixed_quote_shares: 100,
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
    let collateral_amount = u64::try_from(current_debt)
        .unwrap()
        .checked_mul(2)
        .unwrap();
    let mut borrow_position = BorrowPosition {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        base_collateral: 0,
        quote_collateral: 0,
        utilized_base_collateral_for_quote_debt: 0,
        utilized_quote_collateral_for_base_debt: 0,
        fixed_base_shares: 0,
        fixed_quote_shares: 0,
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
                reserved_liability: 0,
            };
            base_side.shares.ylp_supply = debt_live;
            quote_side.reserves = Reserves {
                live_reserve: collateral_cash,
                cash_reserve: collateral_cash,
                reserved_liability: 0,
            };
            quote_side.shares.ylp_supply = collateral_cash;
            debt.base_borrow_index_nad = next_index;
            debt.fixed_base_shares = shares;
            debt.fixed_base_principal = borrow_amount as u128;
            debt.utilized_quote_collateral_for_base_debt = collateral_amount;
            borrow_position.fixed_base_shares = shares;
            borrow_position.quote_collateral = collateral_amount;
            borrow_position.utilized_quote_collateral_for_base_debt = collateral_amount;
        }
        MarketAsset::Quote => {
            base_side.reserves = Reserves {
                live_reserve: collateral_cash,
                cash_reserve: collateral_cash,
                reserved_liability: 0,
            };
            base_side.shares.ylp_supply = collateral_cash;
            quote_side.reserves = Reserves {
                live_reserve: debt_live,
                cash_reserve: debt_cash_after_borrow,
                reserved_liability: 0,
            };
            quote_side.shares.ylp_supply = debt_live;
            debt.quote_borrow_index_nad = next_index;
            debt.fixed_quote_shares = shares;
            debt.fixed_quote_principal = borrow_amount as u128;
            debt.utilized_base_collateral_for_quote_debt = collateral_amount;
            borrow_position.fixed_quote_shares = shares;
            borrow_position.base_collateral = collateral_amount;
            borrow_position.utilized_base_collateral_for_quote_debt = collateral_amount;
        }
    }

    let market = Market {
        version: MARKET_VERSION,
        ylp_mint: Pubkey::new_unique(),
        operator: Pubkey::new_unique(),
        manager: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: valid_config(),
        debt,
        base_hlp_vault: HlpVault::default(),
        quote_hlp_vault: HlpVault::default(),
        risk: Risk::default(),
        insurance: Insurance::default(),
        pending_config: PendingConfigChange::default(),
        pending_operator: PendingAuthorityChange::default(),
        pending_manager: PendingAuthorityChange::default(),
        params_hash: [7; 32],
        last_update_slot: 0,
        reduce_only: false,
        bump: 255,
    };

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

fn position_debt_after(
    market: &Market,
    borrow_position: &BorrowPosition,
    debt_asset: MarketAsset,
) -> u128 {
    match debt_asset {
        MarketAsset::Base => borrow_position.fixed_base_debt(&market.debt).unwrap(),
        MarketAsset::Quote => borrow_position.fixed_quote_debt(&market.debt).unwrap(),
    }
}

fn reserve_pair(market: &Market, asset: MarketAsset) -> (u64, u64) {
    let side = market.side(asset);
    (side.reserves.live_reserve, side.reserves.cash_reserve)
}

#[test]
fn euler_style_incentive_grows_with_health_shortfall() {
    assert_eq!(liquidation_incentive_bps(10_999, 11_000), 100);
    assert_eq!(liquidation_incentive_bps(10_750, 11_000), 250);
    assert_eq!(liquidation_incentive_bps(9_000, 11_000), 500);
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
        let repay_credit = u64::try_from(repay_credit).unwrap();
        let (live_before, cash_before) = reserve_pair(&market, debt_asset);
        let pricing = LiquidationPricing::ReferencePrice {
            debt_per_collateral_price_nad: NAD as u64,
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
        let principal_credit = repay_credit.checked_sub(receipt.interest_paid).unwrap();
        let live_debit = debt_reduction.checked_sub(principal_credit).unwrap();
        let (live_after, cash_after) = reserve_pair(&market, debt_asset);

        prop_assert_eq!(receipt.socialized_loss, 0);
        prop_assert_eq!(receipt.insurance_drawn, 0);
        prop_assert_eq!(live_after, live_before - live_debit);
        prop_assert_eq!(cash_after, cash_before + principal_credit);
        prop_assert!(debt_reduction >= repay_credit);
        market.assert_market_invariants().unwrap();
    }
}

#[test]
fn partial_liquidation_rounding_writeoff_preserves_virtual_reserve_invariant() {
    let debt_asset = MarketAsset::Quote;
    let debt_cash = 28_642_837;
    let borrow_amount = debt_cash * 346 / BPS_DENOMINATOR as u64;
    let (mut market, mut borrow_position) =
        market_with_cash_backed_debt(debt_asset, debt_cash, 1_000_000, borrow_amount, 413);
    let debt_before = position_debt_after(&market, &borrow_position, debt_asset);
    let repay_credit = u64::try_from(debt_before * 205 / BPS_DENOMINATOR as u128).unwrap();
    let (live_before, cash_before) = reserve_pair(&market, debt_asset);
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD as u64,
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
    let principal_credit = repay_credit.checked_sub(receipt.interest_paid).unwrap();
    assert_eq!(debt_reduction, repay_credit + 1);
    assert_eq!(
        market.quote_side.reserves.live_reserve,
        live_before - (debt_reduction - principal_credit)
    );
    assert_eq!(
        market.quote_side.reserves.cash_reserve,
        cash_before + principal_credit
    );
    market.assert_market_invariants().unwrap();
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
    market.insurance.quote_available = insurance_credit;
    let (live_before, cash_before) = reserve_pair(&market, debt_asset);
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD as u64,
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
    assert_eq!(
        market.quote_side.reserves.cash_reserve,
        cash_before + principal_credit
    );
    market.assert_market_invariants().unwrap();
}

#[test]
fn collateral_exhausted_liquidation_socializes_loss_without_breaking_virtual_reserve_invariant() {
    let debt_asset = MarketAsset::Quote;
    let (mut market, mut borrow_position) =
        market_with_cash_backed_debt(debt_asset, 2_000_000, 2_000_000, 100_000, 500);
    borrow_position.base_collateral = 1;
    borrow_position.utilized_base_collateral_for_quote_debt = 1;
    market.debt.utilized_base_collateral_for_quote_debt = 1;
    let debt_before = position_debt_after(&market, &borrow_position, debt_asset);
    let debt_before_u64 = u64::try_from(debt_before).unwrap();
    let repay_credit = debt_before_u64 / 2;
    let max_socialized_loss = debt_before_u64 - repay_credit;
    let (live_before, cash_before) = reserve_pair(&market, debt_asset);
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD as u64,
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
    assert_eq!(
        market.quote_side.reserves.cash_reserve,
        cash_before + principal_credit
    );
    market.assert_market_invariants().unwrap();
}

#[test]
fn insurance_funding_preserves_room_to_restore_health() {
    let config = valid_config();

    assert_eq!(liquidation_insurance_funding_bps(100, &config).unwrap(), 200);
    assert_eq!(liquidation_insurance_funding_bps(500, &config).unwrap(), 200);

    let mut tight_config = valid_config();
    tight_config.market_health_min_bps = 10_250;
    assert_eq!(
        liquidation_insurance_funding_bps(200, &tight_config).unwrap(),
        49
    );
}

#[test]
fn max_repay_caps_liquidation_to_restore_target_health() {
    let (market, borrow_position) = liquidatable_quote_debt_position();
    let incentive_bps = liquidation_incentive_bps(10_900, 11_000);
    let insurance_bps = liquidation_insurance_funding_bps(incentive_bps, &market.config).unwrap();
    let cap = max_repay_to_restore_health_with_pricing(
        &market,
        &borrow_position,
        MarketAsset::Quote,
        incentive_bps + insurance_bps,
        LiquidationPricing::PessimisticReserves,
    )
    .unwrap();

    assert!((15..=16).contains(&cap));
}

#[test]
fn reference_pricing_uses_ema_price_for_collateral_seizure() {
    let (market, _) = liquidatable_quote_debt_position();
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD as u64,
    };

    let seized = collateral_amount_for_debt_value_with_pricing(
        &market,
        MarketAsset::Quote,
        100,
        300,
        pricing,
    )
    .unwrap();
    let bidder_collateral = collateral_amount_for_debt_value_with_pricing(
        &market,
        MarketAsset::Quote,
        100,
        100,
        pricing,
    )
    .unwrap();

    assert_eq!(seized, 103);
    assert_eq!(bidder_collateral, 101);
}

#[test]
fn direct_liquidation_restore_cap_uses_reference_price() {
    let (market, borrow_position) = liquidatable_quote_debt_position();
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD as u64,
    };
    let cap = max_repay_to_restore_health_with_pricing(
        &market,
        &borrow_position,
        MarketAsset::Quote,
        300,
        pricing,
    )
    .unwrap();

    assert!((15..=16).contains(&cap));
}

#[test]
fn max_repay_respects_close_factor_for_deep_partial_liquidation() {
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    borrow_position.base_collateral = 50;
    borrow_position.utilized_base_collateral_for_quote_debt = 50;
    market.debt.utilized_base_collateral_for_quote_debt = 50;
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD as u64,
    };
    let terms = liquidation_terms_with_pricing(
        &market,
        &borrow_position,
        MarketAsset::Quote,
        pricing,
    )
    .unwrap();

    assert_eq!(terms.max_repay_amount, 50);
}

#[test]
fn max_repay_full_closes_when_partial_would_leave_dust() {
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    market.debt.fixed_quote_shares = 2;
    market.debt.fixed_quote_principal = 2;
    market.debt.utilized_base_collateral_for_quote_debt = 1;
    borrow_position.fixed_quote_shares = 2;
    borrow_position.base_collateral = 1;
    borrow_position.utilized_base_collateral_for_quote_debt = 1;
    let pricing = LiquidationPricing::ReferencePrice {
        debt_per_collateral_price_nad: NAD as u64,
    };
    let terms = liquidation_terms_with_pricing(
        &market,
        &borrow_position,
        MarketAsset::Quote,
        pricing,
    )
    .unwrap();

    assert_eq!(terms.max_repay_amount, 2);
}

#[test]
fn liquidation_rejects_repay_above_restore_cap() {
    let (mut market, mut borrow_position) = liquidatable_quote_debt_position();
    let incentive_bps = liquidation_incentive_bps(10_900, 11_000);
    let insurance_bps = liquidation_insurance_funding_bps(incentive_bps, &market.config).unwrap();
    let cap = max_repay_to_restore_health_with_pricing(
        &market,
        &borrow_position,
        MarketAsset::Quote,
        incentive_bps + insurance_bps,
        LiquidationPricing::PessimisticReserves,
    )
    .unwrap();

    let terms = liquidation_terms(&market, &borrow_position, MarketAsset::Quote).unwrap();
    let err = Liquidation::new(MarketAsset::Quote, cap + 1, 0, 0, 0, terms)
        .apply(&mut market, &mut borrow_position)
        .unwrap_err();

    assert_eq!(
        err,
        anchor_lang::prelude::error!(ErrorCode::LiquidationRepayTooLarge)
    );
}

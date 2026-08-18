use super::*;
use crate::state::{FeeProfile, IrmConfig, DEFAULT_DAILY_BORROW_BPS};
use proptest::prelude::*;

fn valid_config() -> MarketConfig {
    MarketConfig {
        swap_fee_bps: 0,
        divergence_fee_share_cap_bps: 0,
        volatility_fee_share_cap_bps: 0,
        target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
        settlement_divergence_bps: BPS_DENOMINATOR,
        ema_half_life_ms: MIN_HALF_LIFE_MS,
        directional_ema_half_life_ms: MIN_HALF_LIFE_MS,
        q_ema_half_life_ms: MIN_HALF_LIFE_MS,
        max_daily_borrow_bps: DEFAULT_DAILY_BORROW_BPS,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: BPS_DENOMINATOR,
        amm: Default::default(),
        irm: Default::default(),
        start_time: 0,
    }
}

#[test]
fn market_mint_domain_requires_all_five_mints_to_be_pairwise_distinct() {
    let distinct = [
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    ];
    Market::validate_mint_domain(distinct[0], distinct[1], distinct[2], distinct[3], distinct[4]).unwrap();

    for duplicate_index in 1..distinct.len() {
        for original_index in 0..duplicate_index {
            let mut colliding = distinct;
            colliding[duplicate_index] = colliding[original_index];
            let error =
                Market::validate_mint_domain(colliding[0], colliding[1], colliding[2], colliding[3], colliding[4])
                    .unwrap_err();
            match error {
                anchor_lang::error::Error::AnchorError(error) => {
                    let expected = if original_index == 0 && duplicate_index == 1 {
                        "InvalidMint"
                    } else {
                        "InvalidLpMintKey"
                    };
                    assert_eq!(error.error_name, expected);
                }
                other => panic!("unexpected error for pair ({original_index}, {duplicate_index}): {other:?}"),
            }
        }
    }
}

#[test]
fn market_mint_domain_rejects_one_mint_for_both_hlp_sides() {
    let shared_hlp_mint = Pubkey::new_unique();
    let error = Market::validate_mint_domain(
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        shared_hlp_mint,
        shared_hlp_mint,
    )
    .unwrap_err();

    match error {
        anchor_lang::error::Error::AnchorError(error) => assert_eq!(error.error_name, "InvalidLpMintKey"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn scheduled_market_activates_at_the_exact_configured_timestamp() {
    let mut market = invariant_market(1_000, 1_000);
    market.config.start_time = 1_000;
    assert_eq!(
        market.assert_started_at(999).unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::MarketNotStarted)
    );
    market.assert_started_at(1_000).unwrap();
}

#[test]
fn ordinary_liquidity_can_seed_before_start_but_reduce_only_still_blocks_it() {
    let mut market = invariant_market(1_000, 1_000);
    market.config.start_time = 1_000;
    let mut futarchy = FutarchyAuthority::initialize(
        Pubkey::new_unique(),
        0,
        0,
        0,
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        BPS_DENOMINATOR,
        0,
        0,
        1,
    )
    .unwrap();

    assert!(market.assert_started_at(999).is_err());
    market
        .assert_liquidity_seeding_available_with_futarchy(&futarchy)
        .unwrap();
    futarchy.global_reduce_only = true;
    assert_eq!(
        market
            .assert_liquidity_seeding_available_with_futarchy(&futarchy)
            .unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::ReduceOnlyMode)
    );
}

fn invariant_market(base_cash: u64, quote_cash: u64) -> Market {
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let mut base_side = MarketSide {
        asset_mint: base_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    base_side.reserves = Reserves {
        live_reserve: base_cash,
        cash_reserve: base_cash,
        ..Reserves::default()
    };
    let mut quote_side = MarketSide {
        asset_mint: quote_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    quote_side.reserves = Reserves {
        live_reserve: quote_cash,
        cash_reserve: quote_cash,
        ..Reserves::default()
    };
    let ylp_supply = base_cash.min(quote_cash).max(1);
    base_side.shares.ylp_supply = ylp_supply;
    quote_side.shares.ylp_supply = ylp_supply;
    let mut market = Market {
        version: MARKET_LAYOUT_VERSION,
        ylp_mint: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: valid_config(),
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
    market
}

fn borrow_position_for_debt(debt_asset: MarketAsset, collateral_amount: u64) -> BorrowPosition {
    let mut position = BorrowPosition {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        base_collateral: 0,
        quote_collateral: 0,
        global_health_base_contribution_for_quote_debt: 0,
        global_health_quote_contribution_for_base_debt: 0,
        base_liquidation_cf_bps: 0,
        quote_liquidation_cf_bps: 0,
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
        MarketAsset::Base => position.quote_collateral = collateral_amount,
        MarketAsset::Quote => position.base_collateral = collateral_amount,
    }
    position
}

fn reserve_pair(market: &Market, asset: MarketAsset) -> (u64, u64) {
    let side = market.side(asset);
    (side.reserves.live_reserve, side.reserves.cash_reserve)
}

fn set_borrow_index(market: &mut Market, asset: MarketAsset, index_nad: u128) {
    match asset {
        MarketAsset::Base => market.debt.base_borrow_index_nad = index_nad,
        MarketAsset::Quote => market.debt.quote_borrow_index_nad = index_nad,
    }
}

fn add_accrued_cash_backed_interest_to_live_reserve(
    market: &mut Market,
    asset: MarketAsset,
    shares: u128,
    principal: u64,
) -> u64 {
    let index = market.debt.borrow_index(asset);
    let current_debt = Debt::shares_to_debt(shares, index).unwrap();
    let accrued_interest = current_debt.checked_sub(principal as u128).unwrap();
    let accrued_interest = u64::try_from(accrued_interest).unwrap();
    let side = market.side_mut(asset);
    side.reserves.live_reserve = side.reserves.live_reserve.checked_add(accrued_interest).unwrap();
    accrued_interest
}

#[test]
fn borrow_preserves_virtual_reserve_as_cash_plus_debt() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    let mut borrow_position = BorrowPosition {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        base_collateral: 0,
        quote_collateral: 250_000,
        global_health_base_contribution_for_quote_debt: 0,
        global_health_quote_contribution_for_base_debt: 0,
        base_liquidation_cf_bps: 0,
        quote_liquidation_cf_bps: 0,
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

    market
        .borrow(&mut borrow_position, MarketAsset::Base, 100_000, 0, 0)
        .unwrap();

    assert_eq!(market.base_side.reserves.live_reserve, 1_000_000);
    assert_eq!(market.base_side.reserves.cash_reserve, 900_000);
    assert_eq!(market.debt.fixed_base_debt().unwrap(), 100_000);
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 100_000);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn borrow_never_exceeds_cash_headroom() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.base_side.reserves.cash_reserve = 10_000;
    let mut position = borrow_position_for_debt(MarketAsset::Base, 500_000);

    let err = market
        .borrow(&mut position, MarketAsset::Base, 10_001, 0, 0)
        .unwrap_err();

    assert_eq!(err, anchor_lang::prelude::error!(ErrorCode::InsufficientBorrowHeadroom));
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 0);
    assert_eq!(position.fixed_base_shares, 0);
}

#[test]
fn liquidation_cf_slippage_protects_borrow_and_withdrawal() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    let mut position = borrow_position_for_debt(MarketAsset::Base, 250_000);

    let borrow_err = market
        .borrow(&mut position, MarketAsset::Base, 100_000, 8_501, 0)
        .unwrap_err();
    assert_eq!(borrow_err, anchor_lang::prelude::error!(ErrorCode::SlippageExceeded));
    assert_eq!(position.fixed_base_shares, 0);

    market
        .borrow(&mut position, MarketAsset::Base, 100_000, 8_500, 0)
        .unwrap();
    let withdrawal_err = market
        .withdraw_collateral(&mut position, MarketAsset::Quote, 1, 8_501)
        .unwrap_err();
    assert_eq!(
        withdrawal_err,
        anchor_lang::prelude::error!(ErrorCode::SlippageExceeded)
    );
    assert_eq!(position.quote_collateral, 250_000);
    assert_eq!(position.base_liquidation_cf_bps, 8_500);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn borrow_preserves_cash_backed_virtual_reserve_invariant_across_assets(
        borrow_base in any::<bool>(),
        base_cash in 1_000_000u64..50_000_000,
        quote_cash in 1_000_000u64..50_000_000,
        borrow_bps in 1u64..=500,
    ) {
        let borrow_asset = if borrow_base {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let collateral_asset = borrow_asset.opposite();
        let mut market = invariant_market(base_cash, quote_cash);
        let debt_cash_before = market.side(borrow_asset).reserves.cash_reserve;
        let debt_live_before = market.side(borrow_asset).reserves.live_reserve;
        let collateral_amount = market.side(collateral_asset).reserves.live_reserve / 2;
        let mut borrow_position =
            borrow_position_for_debt(borrow_asset, collateral_amount.max(1));
        let borrow_amount = debt_cash_before
            .checked_mul(borrow_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u64)
            .unwrap()
            .max(1);

        let receipt = market
            .borrow(
                &mut borrow_position,
                borrow_asset,
                borrow_amount,
                0,
                0,
            )
            .unwrap();

        let (live_after, cash_after) = reserve_pair(&market, borrow_asset);
        prop_assert_eq!(receipt.interest_paid, 0);
        prop_assert_eq!(live_after, debt_live_before);
        prop_assert_eq!(cash_after, debt_cash_before - borrow_amount);
        match borrow_asset {
            MarketAsset::Base => {
                prop_assert_eq!(borrow_position.fixed_base_debt(&market.debt).unwrap(), borrow_amount as u128);
                prop_assert_eq!(market.debt.fixed_base_principal, borrow_amount);
            }
            MarketAsset::Quote => {
                prop_assert_eq!(borrow_position.fixed_quote_debt(&market.debt).unwrap(), borrow_amount as u128);
                prop_assert_eq!(market.debt.fixed_quote_principal, borrow_amount);
            }
        }
        market.assert_market_invariants().unwrap();
    }

    #[test]
    fn repay_preserves_cash_backed_virtual_reserve_invariant_across_principal_and_interest(
        repay_base in any::<bool>(),
        base_cash in 1_000_000u64..50_000_000,
        quote_cash in 1_000_000u64..50_000_000,
        borrow_bps in 1u64..=500,
        interest_bps in 1u128..=2_000,
        repay_bps in 1u128..=10_000,
    ) {
        let repay_asset = if repay_base {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let collateral_asset = repay_asset.opposite();
        let mut market = invariant_market(base_cash, quote_cash);
        let debt_cash_before = market.side(repay_asset).reserves.cash_reserve;
        let collateral_amount = market.side(collateral_asset).reserves.live_reserve / 2;
        let mut borrow_position =
            borrow_position_for_debt(repay_asset, collateral_amount.max(1));
        let borrow_amount = debt_cash_before
            .checked_mul(borrow_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u64)
            .unwrap()
            .max(1);
        market
            .borrow(
                &mut borrow_position,
                repay_asset,
                borrow_amount,
                0,
                0,
            )
            .unwrap();

        let shares = match repay_asset {
            MarketAsset::Base => borrow_position.fixed_base_shares,
            MarketAsset::Quote => borrow_position.fixed_quote_shares,
        };
        let next_index = (NAD as u128)
            .checked_mul((BPS_DENOMINATOR as u128).checked_add(interest_bps).unwrap())
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u128)
            .unwrap();
        set_borrow_index(&mut market, repay_asset, next_index);
        add_accrued_cash_backed_interest_to_live_reserve(
            &mut market,
            repay_asset,
            shares,
            borrow_amount,
        );
        market.assert_virtual_reserve_invariant(repay_asset).unwrap();

        let debt_before = match repay_asset {
            MarketAsset::Base => borrow_position.fixed_base_debt(&market.debt).unwrap(),
            MarketAsset::Quote => borrow_position.fixed_quote_debt(&market.debt).unwrap(),
        };
        let repay_credit = debt_before
            .checked_mul(repay_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u128)
            .unwrap()
            .max(1)
            .min(debt_before);
        let max_repay_credit = u64::try_from(repay_credit).unwrap();
        let repayment = market
            .fixed_repayment_for_max(&borrow_position, repay_asset, max_repay_credit)
            .unwrap();
        let repay_credit = repayment.cash_repaid;
        let aggregate_debt_before = match repay_asset {
            MarketAsset::Base => market.debt.fixed_base_debt().unwrap(),
            MarketAsset::Quote => market.debt.fixed_quote_debt().unwrap(),
        };
        let (live_before, cash_before) = reserve_pair(&market, repay_asset);

        let receipt = market
            .repay(&mut borrow_position, repay_asset, repay_credit)
            .unwrap();

        let (live_after, cash_after) = reserve_pair(&market, repay_asset);
        let aggregate_debt_after = match repay_asset {
            MarketAsset::Base => market.debt.fixed_base_debt().unwrap(),
            MarketAsset::Quote => market.debt.fixed_quote_debt().unwrap(),
        };
        let principal_paid = repay_credit.checked_sub(receipt.interest_paid).unwrap();
        let debt_reduction = receipt.debt_delta.unsigned_abs();
        let aggregate_debt_reduction = u64::try_from(aggregate_debt_before - aggregate_debt_after).unwrap();
        let live_debit = aggregate_debt_reduction.checked_sub(principal_paid).unwrap();
        prop_assert_eq!(live_after, live_before - live_debit);
        prop_assert_eq!(cash_after, cash_before + principal_paid);
        prop_assert!(receipt.interest_paid <= repay_credit);
        prop_assert!(repay_credit <= max_repay_credit);
        prop_assert!(debt_reduction.abs_diff(repay_credit) <= 1);
        prop_assert_eq!(aggregate_debt_reduction, repay_credit);
        market.assert_market_invariants().unwrap();
    }
}

#[test]
fn partial_repay_charges_the_aggregate_delta_without_rounding_writeoff() {
    let repay_asset = MarketAsset::Quote;
    let mut market = invariant_market(1_000_000, 28_642_837);
    let mut borrow_position = borrow_position_for_debt(repay_asset, 500_000);
    let borrow_amount = 28_642_837 * 346 / BPS_DENOMINATOR as u64;
    market
        .borrow(&mut borrow_position, repay_asset, borrow_amount, 0, 0)
        .unwrap();

    let shares = borrow_position.fixed_quote_shares;
    let next_index = (NAD as u128) * 10_413 / BPS_DENOMINATOR as u128;
    set_borrow_index(&mut market, repay_asset, next_index);
    add_accrued_cash_backed_interest_to_live_reserve(&mut market, repay_asset, shares, borrow_amount);
    market.assert_virtual_reserve_invariant(repay_asset).unwrap();

    let debt_before = borrow_position.fixed_quote_debt(&market.debt).unwrap();
    let max_repay_credit = u64::try_from(debt_before * 205 / BPS_DENOMINATOR as u128).unwrap();
    let repay_credit = market
        .fixed_repayment_for_max(&borrow_position, repay_asset, max_repay_credit)
        .unwrap()
        .cash_repaid;
    let aggregate_debt_before = market.debt.fixed_quote_debt().unwrap();
    let (live_before, cash_before) = reserve_pair(&market, repay_asset);

    let receipt = market.repay(&mut borrow_position, repay_asset, repay_credit).unwrap();

    let (live_after, cash_after) = reserve_pair(&market, repay_asset);
    let aggregate_debt_after = market.debt.fixed_quote_debt().unwrap();
    let principal_paid = repay_credit.checked_sub(receipt.interest_paid).unwrap();
    let aggregate_debt_reduction = u64::try_from(aggregate_debt_before - aggregate_debt_after).unwrap();
    assert_eq!(receipt.cash_repaid, repay_credit);
    assert_eq!(aggregate_debt_reduction, repay_credit);
    assert_eq!(live_after, live_before - (aggregate_debt_reduction - principal_paid));
    assert_eq!(cash_after, cash_before + principal_paid);
    market.assert_market_invariants().unwrap();
}

#[test]
fn partial_repay_uses_aggregate_debt_delta_with_multiple_positions() {
    let repay_asset = MarketAsset::Quote;
    let mut market = invariant_market(500_000_100, 500_000_100);
    market.config.borrow_market_health_floor_bps = 9_000;
    let mut first = borrow_position_for_debt(repay_asset, 150_000_000);
    let mut second = borrow_position_for_debt(repay_asset, 150_000_000);
    let borrow_amount = 50_000_003;
    market.borrow(&mut first, repay_asset, borrow_amount, 0, 0).unwrap();
    market.borrow(&mut second, repay_asset, borrow_amount, 0, 0).unwrap();

    let next_index = (NAD as u128) * 10_413 / BPS_DENOMINATOR as u128;
    set_borrow_index(&mut market, repay_asset, next_index);
    add_accrued_cash_backed_interest_to_live_reserve(
        &mut market,
        repay_asset,
        first.fixed_quote_shares + second.fixed_quote_shares,
        borrow_amount * 2,
    );
    market.assert_virtual_reserve_invariant(repay_asset).unwrap();

    let repay_credit = market
        .fixed_repayment_for_max(&first, repay_asset, 25_000_004)
        .unwrap()
        .cash_repaid;
    market.repay(&mut first, repay_asset, repay_credit).unwrap();

    market.assert_virtual_reserve_invariant(repay_asset).unwrap();
}

#[test]
fn virtual_reserve_sums_fixed_and_isolated_debt_ledgers_separately() {
    let mut market = invariant_market(1_000, 1_000);
    market.debt.base_borrow_index_nad = (NAD as u128) * 3 / 2;
    market.debt.fixed_base_shares = 1;
    market.debt.isolated_base_shares = 1;
    market.base_side.reserves.live_reserve = 1_002;

    assert_eq!(
        total_cash_backed_borrowed(&market, MarketAsset::Base, market.debt.base_borrow_index_nad).unwrap(),
        2
    );
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

#[test]
fn borrower_risk_valuation_uses_q_ema_depth_cap() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.risk = Risk {
        base_price_ema_nad: NAD,
        quote_price_ema_nad: NAD,
        directional_base_price_ema_nad: NAD,
        directional_quote_price_ema_nad: NAD,
        q_ema_nad: 100_000_u128 * NAD as u128,
        ..Risk::default()
    };

    let value = market
        .collateral_value_nad(MarketAsset::Base, 50_000, &market.risk)
        .unwrap();
    let expected = crate::math::cpmm_amount_out_nad(
        100_000_u128 * NAD as u128,
        100_000_u128 * NAD as u128,
        50_000_u128 * NAD as u128,
    )
    .unwrap();
    let live_depth_value = crate::math::cpmm_amount_out_nad(
        1_000_000_u128 * NAD as u128,
        1_000_000_u128 * NAD as u128,
        50_000_u128 * NAD as u128,
    )
    .unwrap();

    assert!(value > 0);
    // The explicit inverse is intentionally pessimistic and need not equal
    // the old synthetic-CPMM Q reconstruction.
    assert!(value < live_depth_value);
}

#[test]
fn reconstructed_risk_curve_caps_q_by_sudden_current_drawdown() {
    let q_ema_nad = 1_000_000_u128 * NAD as u128;
    let high_risk = Risk {
        base_price_ema_nad: NAD,
        quote_price_ema_nad: NAD,
        directional_base_price_ema_nad: NAD,
        directional_quote_price_ema_nad: NAD,
        cached_q_nad: q_ema_nad,
        q_ema_nad,
        ..Risk::default()
    };
    let low_risk = Risk {
        cached_q_nad: q_ema_nad / 10,
        ..high_risk
    };
    let market = invariant_market(1_000_000, 1_000_000);
    let (high_base, high_quote) = market
        .pessimistic_virtual_reserves_nad(MarketAsset::Base, &high_risk, true)
        .unwrap();
    let (low_base, low_quote) = market
        .pessimistic_virtual_reserves_nad(MarketAsset::Base, &low_risk, true)
        .unwrap();
    assert!(low_base < high_base);
    assert!(low_quote < high_quote);
}

#[test]
fn daily_borrow_bucket_use_conservative_q_at_current_spot_ratio() {
    let mut market = invariant_market(4_000_000, 1_000_000);
    market.risk.q_ema_nad = 1_000_000_u128 * NAD as u128;

    let (base_depth, quote_depth) = market.conservative_risk_reserve_depths(&market.risk).unwrap();
    assert_eq!(market.daily_limit_for_side(MarketAsset::Base, 2_000).unwrap(), base_depth / 5);
    assert_eq!(market.daily_limit_for_side(MarketAsset::Quote, 2_000).unwrap(), quote_depth / 5);
}

#[test]
fn daily_borrow_bucket_track_post_swap_reserve_ratio() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.risk.q_ema_nad = 1_000_000_u128 * NAD as u128;
    let amount_in = 250_000;
    let amount_out = crate::math::cpmm_amount_out(1_000_000, 1_000_000, amount_in).unwrap();
    market.base_side.reserves.live_reserve += amount_in;
    market.quote_side.reserves.live_reserve -= amount_out;

    assert_eq!(market.base_side.reserves.live_reserve, 1_250_000);
    assert_eq!(market.quote_side.reserves.live_reserve, 800_000);
    let (base_depth, quote_depth) = market.conservative_risk_reserve_depths(&market.risk).unwrap();
    assert_eq!(market.daily_limit_for_side(MarketAsset::Base, 1_000).unwrap(), base_depth / 10);
    assert_eq!(market.daily_limit_for_side(MarketAsset::Quote, 1_000).unwrap(), quote_depth / 10);
}

#[test]
fn daily_borrow_bucket_use_live_depth_when_q_ema_is_empty_or_above_spot() {
    let mut market = invariant_market(800_000, 1_200_000);

    assert_eq!(market.daily_limit_for_side(MarketAsset::Base, 2_500).unwrap(), 200_000);
    assert!(market.daily_limit_for_side(MarketAsset::Quote, 2_500).unwrap().abs_diff(300_000) <= 1);

    market.risk.q_ema_nad = 2_000_000_u128 * NAD as u128;
    assert_eq!(market.daily_limit_for_side(MarketAsset::Base, 2_500).unwrap(), 200_000);
    assert!(market.daily_limit_for_side(MarketAsset::Quote, 2_500).unwrap().abs_diff(300_000) <= 1);
}

#[test]
fn daily_borrow_bucket_follow_q_drawdown_growth_and_proportional_liquidity() {
    let mut market = invariant_market(2_000_000, 2_000_000);
    market.risk.q_ema_nad = 1_000_000_u128 * NAD as u128;

    let initial = market.daily_limit_for_side(MarketAsset::Base, 1_000).unwrap();
    assert!(initial > 0);

    market.base_side.reserves.live_reserve = 500_000;
    market.quote_side.reserves.live_reserve = 500_000;
    assert!(market.daily_limit_for_side(MarketAsset::Base, 1_000).unwrap() < initial);

    market.base_side.reserves.live_reserve = 2_000_000;
    market.quote_side.reserves.live_reserve = 500_000;
    assert!(market.daily_limit_for_side(MarketAsset::Base, 1_000).unwrap() <= 200_000);
    assert!(market.daily_limit_for_side(MarketAsset::Quote, 1_000).unwrap() <= 50_000);
}

#[test]
fn daily_borrow_bucket_respect_mixed_token_decimals() {
    let mut market = invariant_market(1_000_000_000, 2_000_000_000_000);
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 9;
    market.amm = AmmState::default();
    market.risk = Risk::default();
    market.prepare_amm_for_swap(0).unwrap();
    market.refresh_risk().unwrap();

    assert_eq!(
        market.daily_limit_for_side(MarketAsset::Base, 1_000).unwrap(),
        100_000_000
    );
    assert!(
        market
            .daily_limit_for_side(MarketAsset::Quote, 1_000)
            .unwrap()
            .abs_diff(200_000_000_000)
            <= 100
    );
}

#[test]
fn global_health_contribution_is_debt_capped_and_rounded_down() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.refresh_risk().unwrap();

    assert_eq!(
        market
            .debt_capped_global_health_contribution(MarketAsset::Base, 100_000, 1_000_000, &market.risk,)
            .unwrap(),
        150_000
    );
    assert_eq!(
        market
            .debt_capped_global_health_contribution(MarketAsset::Base, 100_000, 120_000, &market.risk,)
            .unwrap(),
        120_000
    );
    assert_eq!(
        market
            .debt_capped_global_health_contribution(MarketAsset::Base, 1, 10, &market.risk)
            .unwrap(),
        1
    );
}

#[test]
fn global_health_cap_remains_bounded_after_collateral_appreciates() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.debt.fixed_base_shares = 100_000;
    market.debt.fixed_base_principal = 100_000;
    market.debt.global_health_quote_contribution_for_base_debt = 150_000;
    market.risk = Risk {
        base_price_ema_nad: NAD / 2,
        quote_price_ema_nad: NAD * 2,
        directional_base_price_ema_nad: NAD / 2,
        directional_quote_price_ema_nad: NAD * 2,
        q_ema_nad: 1_000_000_u128 * NAD as u128,
        ..Risk::default()
    };

    let health = market.market_health().unwrap();

    assert!(health.base_debt_health_bps <= 15_000);
    assert!(health.effective_base_debt_nad > 0);
}

#[test]
fn external_fixed_debt_excludes_only_the_acting_position() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.debt.base_borrow_index_nad = (NAD as u128) * 3 / 2;
    market.debt.fixed_base_shares = 150_000;
    let mut position = borrow_position_for_debt(MarketAsset::Base, 250_000);
    position.fixed_base_shares = 100_000;

    assert_eq!(
        market.external_fixed_debt_nad(&position, MarketAsset::Base).unwrap(),
        75_000_u128 * NAD as u128
    );
}

#[test]
fn deposit_and_repay_update_contribution_without_floating_cf() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.borrow_market_health_floor_bps = 11_000;
    let mut position = borrow_position_for_debt(MarketAsset::Base, 250_000);

    market
        .borrow(&mut position, MarketAsset::Base, 100_000, 8_000, 0)
        .unwrap();
    let stored_cf = position.base_liquidation_cf_bps;
    assert_eq!(stored_cf, 8_500);
    assert_eq!(position.global_health_quote_contribution_for_base_debt, 150_000);

    market
        .deposit_collateral(&mut position, MarketAsset::Quote, 100_000)
        .unwrap();
    assert_eq!(position.base_liquidation_cf_bps, stored_cf);
    assert_eq!(position.global_health_quote_contribution_for_base_debt, 150_000);

    market.repay(&mut position, MarketAsset::Base, 50_000).unwrap();
    assert_eq!(position.base_liquidation_cf_bps, stored_cf);
    assert_eq!(position.global_health_quote_contribution_for_base_debt, 75_000);

    market.repay(&mut position, MarketAsset::Base, 50_000).unwrap();
    assert_eq!(position.base_liquidation_cf_bps, 0);
    assert_eq!(position.global_health_quote_contribution_for_base_debt, 0);
    assert_eq!(market.debt.global_health_quote_contribution_for_base_debt, 0);
}

#[test]
fn withdrawal_uses_stored_terms_without_enforcing_global_floor() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.borrow_market_health_floor_bps = 11_000;
    let mut position = borrow_position_for_debt(MarketAsset::Base, 250_000);
    market
        .borrow(&mut position, MarketAsset::Base, 100_000, 0, 0)
        .unwrap();
    let stored_cf = position.base_liquidation_cf_bps;

    market.debt.global_health_quote_contribution_for_base_debt = 100_000;
    position.global_health_quote_contribution_for_base_debt = 100_000;
    assert!(market.market_health().unwrap().base_debt_health_bps < 11_000);

    market
        .withdraw_collateral(&mut position, MarketAsset::Quote, 100_000, stored_cf)
        .unwrap();
    assert_eq!(position.quote_collateral, 150_000);
    assert_eq!(position.base_liquidation_cf_bps, stored_cf);
}

#[test]
fn interest_growth_reduces_global_health_without_changing_stored_cf() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.borrow_market_health_floor_bps = 11_000;
    let mut position = borrow_position_for_debt(MarketAsset::Base, 300_000);
    market
        .borrow(&mut position, MarketAsset::Base, 100_000, 0, 0)
        .unwrap();
    let stored_cf = position.base_liquidation_cf_bps;
    let contribution = position.global_health_quote_contribution_for_base_debt;
    let health_before = market.market_health().unwrap().base_debt_health_bps;

    market.debt.base_borrow_index_nad = (NAD as u128) * 6 / 5;
    let health_after = market.market_health().unwrap().base_debt_health_bps;

    assert!(health_after < health_before);
    assert_eq!(position.base_liquidation_cf_bps, stored_cf);
    assert_eq!(position.global_health_quote_contribution_for_base_debt, contribution);
}

#[test]
fn alice_exit_does_not_change_bob_terms_and_low_health_pauses_later_borrowing() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.borrow_market_health_floor_bps = 11_000;
    let mut bob = borrow_position_for_debt(MarketAsset::Base, 30_000);
    let mut alice = borrow_position_for_debt(MarketAsset::Base, 300_000);

    market.borrow(&mut bob, MarketAsset::Base, 20_000, 0, 0).unwrap();
    market
        .borrow(&mut alice, MarketAsset::Base, 100_000, 0, 0)
        .unwrap();
    let bob_cf = bob.base_liquidation_cf_bps;

    let aggregate_shares = market.debt.fixed_base_shares;
    market.debt.base_borrow_index_nad = (NAD as u128) * 3 / 2;
    add_accrued_cash_backed_interest_to_live_reserve(&mut market, MarketAsset::Base, aggregate_shares, 120_000);

    market.repay(&mut alice, MarketAsset::Base, 150_000).unwrap();
    market
        .withdraw_collateral(&mut alice, MarketAsset::Quote, 300_000, 0)
        .unwrap();

    assert_eq!(alice.quote_collateral, 0);
    assert_eq!(alice.global_health_quote_contribution_for_base_debt, 0);
    assert_eq!(bob.base_liquidation_cf_bps, bob_cf);
    assert!(market.market_health().unwrap().base_debt_health_bps < 11_000);

    let mut charlie = borrow_position_for_debt(MarketAsset::Base, 100_000);
    let err = market
        .borrow(&mut charlie, MarketAsset::Base, 10_000, 0, 0)
        .unwrap_err();
    assert_eq!(err, anchor_lang::prelude::error!(ErrorCode::InsufficientMarketHealth));
    assert_eq!(bob.base_liquidation_cf_bps, bob_cf);
}

#[test]
fn dynamic_terms_are_decimal_invariant() {
    let mut market = invariant_market(1_000_000_000, 1_000_000_000_000);
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 9;
    market.amm = AmmState::default();
    market.risk = Risk::default();
    market.prepare_amm_for_swap(0).unwrap();
    market.refresh_risk().unwrap();

    let terms = market
        .dynamic_borrow_terms(MarketAsset::Base, 500_000_000_000, 0, 0, 0, &market.risk)
        .unwrap();

    assert!(terms.max_debt > 0 && terms.max_debt <= 500_000_000);
    assert_eq!(terms.max_cf_bps, 8_075);
    assert_eq!(terms.liquidation_cf_bps, 8_500);
}

proptest! {
    #[test]
    fn splitting_positions_cannot_increase_global_health_contribution(
        first_debt in 1_u64..500_000,
        second_debt in 1_u64..500_000,
        first_collateral in 0_u64..1_000_000,
        second_collateral in 0_u64..1_000_000,
    ) {
        let mut market = invariant_market(2_000_000, 2_000_000);
        market.refresh_risk().unwrap();
        let first = market
            .debt_capped_global_health_contribution(
                MarketAsset::Base,
                first_debt as u128,
                first_collateral,
                &market.risk,
            )
            .unwrap();
        let second = market
            .debt_capped_global_health_contribution(
                MarketAsset::Base,
                second_debt as u128,
                second_collateral,
                &market.risk,
            )
            .unwrap();
        let combined = market
            .debt_capped_global_health_contribution(
                MarketAsset::Base,
                (first_debt + second_debt) as u128,
                first_collateral + second_collateral,
                &market.risk,
            )
            .unwrap();

        prop_assert!(first + second <= combined);
    }

    #[test]
    fn conservative_q_depth_and_daily_limit_never_exceed_live_inventory(
        base in 1_000_u64..1_000_000_000,
        quote in 1_000_u64..1_000_000_000,
        q_scale_bps in 1_u128..20_001,
        limit_bps in 0_u16..=BPS_DENOMINATOR,
    ) {
        let mut market = invariant_market(base, quote);
        let spot_q = market
            .amm
            .explicit_curve_cache
            .tail_liquidity
            .checked_add(market.amm.explicit_curve_cache.concentrated_liquidity)
            .unwrap();
        market.risk.q_ema_nad = (spot_q / BPS_DENOMINATOR as u128)
            .checked_mul(q_scale_bps)
            .unwrap();

        let (base_depth, quote_depth) = market
            .conservative_risk_reserve_depths(&market.risk)
            .unwrap();
        prop_assert!(base_depth <= base);
        prop_assert!(quote_depth <= quote);
        prop_assert!(market.daily_limit_for_side(MarketAsset::Base, limit_bps).unwrap() <= base);
        prop_assert!(market.daily_limit_for_side(MarketAsset::Quote, limit_bps).unwrap() <= quote);
    }
}

#[test]
fn repay_routes_interest_out_without_breaking_virtual_reserve_invariant() {
    let mut market = invariant_market(900, 1_000);
    market.base_side.reserves.live_reserve = 1_010;
    market.base_side.shares.ylp_supply = 1_010;
    market.quote_side.shares.ylp_supply = 1_010;
    market.amm = AmmState::default();
    market.prepare_amm_for_swap(0).unwrap();
    market.debt.base_borrow_index_nad = (NAD as u128) * 11 / 10;
    market.debt.fixed_base_shares = 100;
    market.debt.fixed_base_principal = 100;
    let mut borrow_position = BorrowPosition {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        base_collateral: 0,
        quote_collateral: 0,
        global_health_base_contribution_for_quote_debt: 0,
        global_health_quote_contribution_for_base_debt: 0,
        base_liquidation_cf_bps: 0,
        quote_liquidation_cf_bps: 0,
        base_referral_partner: Pubkey::new_unique(),
        quote_referral_partner: Pubkey::default(),
        base_referral_interest_share_bps: 2_500,
        quote_referral_interest_share_bps: 0,
        fixed_base_shares: 100,
        fixed_quote_shares: 0,
        auction_debt_asset: u8::MAX,
        auction_start_time: 0,
        auction_start_price_nad: 0,
        auction_floor_price_nad: 0,
        bump: 255,
    };

    let receipt = market.repay(&mut borrow_position, MarketAsset::Base, 110).unwrap();

    assert_eq!(receipt.interest_paid, 10);
    assert_eq!(market.base_side.reserves.live_reserve, 1_000);
    assert_eq!(market.base_side.reserves.cash_reserve, 1_000);
    assert_eq!(market.debt.fixed_base_debt().unwrap(), 0);
    assert_eq!(borrow_position.base_referral_partner, Pubkey::default());
    assert_eq!(borrow_position.base_referral_interest_share_bps, 0);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

#[test]
fn governance_utilization_counts_every_debt_ledger_and_is_strict_at_eighty_percent() {
    let mut market = invariant_market(1_000, 1_000);
    market.debt.fixed_base_shares = 300;
    market.debt.isolated_base_shares = 200;
    // Base funding debt is held by the opposite (quote) hLP aggregate vault.
    market.quote_hlp_vault.debt_shares = 300;
    market.base_side.reserves.cash_reserve = 200;

    assert_eq!(market.lending_utilization_bps(MarketAsset::Base).unwrap(), 8_000);
    assert_eq!(
        market.assert_parameter_execution_utilization().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::UtilizationGuardExceeded)
    );

    market.base_side.reserves.cash_reserve = 201;
    assert!(market.lending_utilization_bps(MarketAsset::Base).unwrap() < 8_000);
    market.assert_parameter_execution_utilization().unwrap();

    market.debt.fixed_quote_shares = 800;
    market.quote_side.reserves.cash_reserve = 200;
    assert_eq!(
        market.assert_parameter_execution_utilization().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::UtilizationGuardExceeded)
    );
}

#[test]
fn typed_parameter_execution_changes_only_one_family_and_revision() {
    let mut market = invariant_market(1_000_000, 1_000_000);

    let fee = FeeProfile {
        base_fee_bps: 100,
        divergence_fee_share_cap_bps: 1_000,
        volatility_fee_share_cap_bps: 500,
        divergence_fee_coefficient_nad: NAD,
        volatility_fee_coefficient_nad: 0,
        volatility_half_life_ms: MIN_HALF_LIFE_MS,
        volatility_shock_cap_nad: 0,
        volatility_accumulator_cap_nad: 0,
        launch_fee_start_bps: 500,
        launch_fee_duration_seconds: 3_600,
        launch_fee_decay_mode: LAUNCH_FEE_DECAY_LINEAR,
        launch_rate_limit_asset: LAUNCH_RATE_LIMIT_ASSET_BASE,
        launch_rate_limit_reference_nad: NAD,
        launch_rate_limit_increment_bps: 100,
        launch_rate_limit_max_fee_bps: 2_000,
        launch_rate_limit_duration_seconds: 3_600,
        ..FeeProfile::default()
    };
    let before_fee = market.config;
    market
        .execute_parameter_update(&MarketParameterUpdate::Fee(fee), 1)
        .unwrap();
    assert_eq!(market.config.fee_profile(), fee);
    assert_eq!(market.config.max_daily_borrow_bps, before_fee.max_daily_borrow_bps);
    assert_eq!(market.parameter_revisions, [1, 0, 0, 0, 0, 0, 0]);

    let irm = IrmConfig {
        target_utilization_bps: 6_500,
        curve_steepness_nad: 6 * NAD,
        adjustment_speed_per_year: 12,
    };
    market
        .execute_parameter_update(&MarketParameterUpdate::Irm(irm), 2)
        .unwrap();
    assert_eq!(market.config.irm, irm);
    assert_eq!(market.config.fee_profile(), fee);
    assert_eq!(market.parameter_revisions, [1, 0, 1, 0, 0, 0, 0]);

    market
        .execute_parameter_update(
            &MarketParameterUpdate::EmaHalfLives {
                price_ms: 120_000,
                directional_price_ms: 180_000,
                q_ms: 240_000,
                center_price_ms: 300_000,
            },
            3,
        )
        .unwrap();
    assert_eq!(market.config.ema_half_life_ms, 120_000);
    assert_eq!(market.config.directional_ema_half_life_ms, 180_000);
    assert_eq!(market.config.q_ema_half_life_ms, 240_000);
    assert_eq!(market.config.amm.center_ema_half_life_ms, 300_000);
    assert_eq!(market.parameter_revisions, [1, 0, 1, 1, 0, 0, 0]);

    market
        .execute_parameter_update(
            &MarketParameterUpdate::DailyBorrowLimit {
                max_daily_borrow_bps: 3_000,
            },
            4,
        )
        .unwrap();
    assert_eq!(market.config.max_daily_borrow_bps, 3_000);
    assert_eq!(market.parameter_revisions, [1, 0, 1, 1, 1, 0, 0]);

    market
        .execute_parameter_update(
            &MarketParameterUpdate::CenterController {
                adjustment_threshold_nad: NAD / 100,
                adjustment_step_nad: NAD / 1_000,
                min_adjustment_interval_slots: 100,
            },
            5,
        )
        .unwrap();
    assert_eq!(market.config.amm.adjustment_threshold_nad, NAD / 100);
    assert_eq!(market.config.amm.adjustment_step_nad, NAD / 1_000);
    assert_eq!(market.config.amm.min_adjustment_interval_slots, 100);
    assert_eq!(market.parameter_revisions, [1, 0, 1, 1, 1, 1, 0]);
}

#[test]
fn fee_compounding_rate_is_governed_from_zero_through_one_hundred_percent() {
    let config = invariant_market(1_000_000, 1_000_000).config;
    for compounding_fee_bps in [0, 4_000, BPS_DENOMINATOR] {
        let mut profile = config.fee_profile();
        profile.compounding_fee_bps = compounding_fee_bps;
        profile.validate().unwrap();
        let mut updated = config;
        updated.apply_fee_profile(profile).unwrap();
        assert_eq!(updated.amm.compounding_fee_bps, compounding_fee_bps);
    }

    let mut invalid = config.fee_profile();
    invalid.compounding_fee_bps = BPS_DENOMINATOR + 1;
    assert_eq!(
        invalid.validate().unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InvalidMarketConfig)
    );
}

#[test]
fn concentration_execution_reconstructs_the_selected_explicit_shape() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.finalize_amm_transition_and_observe_risk(1).unwrap();
    market
        .execute_parameter_update(
            &MarketParameterUpdate::Concentration {
                range_width_nad: 2 * NAD,
                concentrated_liquidity_share_nad: 9 * NAD / 10,
            },
            2,
        )
        .unwrap();

    assert_eq!(market.config.amm.range_width_nad, 2 * NAD);
    assert_eq!(market.config.amm.concentrated_liquidity_share_nad, 9 * NAD / 10);
    assert_eq!(market.amm.explicit_curve_cache.range_width_nad, 2 * NAD);
    assert_eq!(market.parameter_revisions, [0, 1, 0, 0, 0, 0, 0]);
}

#[test]
fn active_or_residual_hlp_allows_an_atomic_concentration_update() {
    let update = MarketParameterUpdate::Concentration {
        range_width_nad: 2 * NAD,
        concentrated_liquidity_share_nad: 9 * NAD / 10,
    };

    for mut market in [
        {
            let mut active = invariant_market(1_000_000, 1_000_000);
            active.finalize_amm_transition_and_observe_risk(0).unwrap();
            active.base_hlp_vault.hlp_supply = 1;
            active
        },
        {
            let mut residual = invariant_market(1_000_000, 1_000_000);
            residual.finalize_amm_transition_and_observe_risk(0).unwrap();
            residual.quote_hlp_vault.residual_exposure = 1;
            residual
        },
    ] {
        market.execute_parameter_update(&update, 1).unwrap();
        assert_eq!(market.amm.explicit_curve_cache.range_width_nad, 2 * NAD);
    }
}

#[test]
fn utilization_rejection_rolls_back_old_parameter_checkpointing() {
    let mut market = invariant_market(1_000, 1_000);
    market.debt.fixed_base_shares = 800;
    market.debt.fixed_base_principal = 800;
    market.base_side.reserves.cash_reserve = 200;
    let before_market = market.try_to_vec().unwrap();

    let error = market
        .execute_parameter_update(
            &MarketParameterUpdate::DailyBorrowLimit {
                max_daily_borrow_bps: 1_000,
            },
            MS_PER_YEAR / TARGET_MS_PER_SLOT,
        )
        .unwrap_err();

    assert_eq!(error, anchor_lang::prelude::error!(ErrorCode::UtilizationGuardExceeded));
    assert_eq!(market.try_to_vec().unwrap(), before_market);
}

#[test]
fn daily_borrow_rate_change_checkpoints_both_buckets_under_the_old_rate() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    assert_eq!(market.config.max_daily_borrow_bps, 2_000);
    market.base_side.daily_borrow_bucket.borrowed_bucket = 200_000;
    market.quote_side.daily_borrow_bucket.borrowed_bucket = 150_000;
    let half_day_slot = MS_PER_DAY / TARGET_MS_PER_SLOT / 2;

    market
        .execute_parameter_update(
            &MarketParameterUpdate::DailyBorrowLimit {
                max_daily_borrow_bps: 3_000,
            },
            half_day_slot,
        )
        .unwrap();

    // The old 20%/day absolute rate is 200_000 atoms/day. Applying the new
    // 30% rate retroactively would have released 150_000 instead of 100_000.
    assert_eq!(market.base_side.daily_borrow_bucket.borrowed_bucket, 100_000);
    assert_eq!(market.quote_side.daily_borrow_bucket.borrowed_bucket, 50_000);
    assert_eq!(market.base_side.daily_borrow_bucket.last_decay_slot, half_day_slot);
    assert_eq!(market.quote_side.daily_borrow_bucket.last_decay_slot, half_day_slot);
    assert_eq!(market.config.max_daily_borrow_bps, 3_000);
}

#[test]
fn insurance_draws_share_hard_event_and_daily_budgets() {
    let mut insurance = Insurance {
        base_available: 1_000,
        ..Insurance::default()
    };
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, 1).unwrap(), 200);
    insurance.consume_draw(MarketAsset::Base, 200, 1).unwrap();
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, 2).unwrap(), 160);
    insurance.consume_draw(MarketAsset::Base, 160, 2).unwrap();
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, 3).unwrap(), 128);
    insurance.consume_draw(MarketAsset::Base, 128, 3).unwrap();

    // The event cap would now permit 102, but only 12 remains under the
    // 50%-of-opening-backing daily ceiling.
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, 4).unwrap(), 12);
    insurance.consume_draw(MarketAsset::Base, 12, 4).unwrap();
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, 5).unwrap(), 0);
    assert_eq!(insurance.base_available, 500);

    insurance.credit(MarketAsset::Base, 100, 6).unwrap();
    assert_eq!(insurance.base_available, 600);
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, 6).unwrap(), 50);

    let next_window = 1 + INSURANCE_DRAW_WINDOW_SLOTS;
    assert_eq!(insurance.draw_capacity(MarketAsset::Base, next_window).unwrap(), 120);
}

#[test]
fn insurance_governance_can_only_tighten_below_protocol_ceilings() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    let too_large = MarketParameterUpdate::InsuranceDrawCaps {
        per_event_bps: MAX_INSURANCE_DRAW_PER_EVENT_BPS + 1,
        per_day_bps: MAX_INSURANCE_DRAW_PER_DAY_BPS,
    };
    assert!(market.validate_parameter_update(&too_large).is_err());

    market
        .execute_parameter_update(
            &MarketParameterUpdate::InsuranceDrawCaps {
                per_event_bps: 1_000,
                per_day_bps: 3_000,
            },
            1,
        )
        .unwrap();
    assert_eq!(market.insurance.per_event_draw_bps, 1_000);
    assert_eq!(market.insurance.per_day_draw_bps, 3_000);
    assert_eq!(market.parameter_revisions, [0, 0, 0, 0, 0, 0, 1]);
}

#[test]
fn hlp_stop_rate_signal_checkpoints_the_opposite_asset_apr() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.base_hlp_vault.hlp_supply = 100;
    market.base_hlp_vault.debt_shares = Debt::debt_to_shares(100_000, market.debt.quote_borrow_index_nad).unwrap();
    market.base_hlp_vault.debt_principal = 100_000;

    market.accrue_interest_to_slot(100).unwrap();
    let signal = market.base_hlp_vault.funding_apr_ema_nad;
    assert!(signal > 0);
    assert_eq!(market.base_hlp_vault.funding_apr_ema_last_slot, 100);
    assert_eq!(market.hlp_funding_apr_ema_nad(MarketAsset::Base).unwrap(), signal);
    assert_eq!(market.quote_hlp_vault.funding_apr_ema_nad, 0);

    // Zero is a genuine observation: it decays the twelve-hour signal rather
    // than clearing it instantly and making Stop Rate orders spike-sensitive.
    market.debt.quote_rate_at_target_nad = 0;
    let half_life_slots = HLP_FUNDING_APR_EMA_HALF_LIFE_MS / TARGET_MS_PER_SLOT;
    market.accrue_interest_to_slot(100 + half_life_slots).unwrap();
    let decayed = market.base_hlp_vault.funding_apr_ema_nad;
    assert!(decayed > 0);
    assert!(decayed < signal);
}

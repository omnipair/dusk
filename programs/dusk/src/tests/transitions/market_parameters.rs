use super::*;
use crate::state::{FeeProfile, IrmConfig, DEFAULT_DAILY_BORROW_BPS};
use crate::transitions::lending::total_cash_backed_borrowed;
use crate::transitions::{LendingCollateralFees, LeverageCollateralFee};
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
        curve_depth_ema_half_life_ms: MIN_HALF_LIFE_MS,
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
        Some(0),
        Some(0),
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

/// Internal amount of whole units of the zero-decimal fixture assets.
fn zero_decimal_nad(units: u128) -> u128 {
    Market::current_layout().normalize_amount(units, 0).unwrap()
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
        live_reserve: u128::from(base_cash),
        cash_reserve: base_cash,
        ..Reserves::default()
    };
    let mut quote_side = MarketSide {
        asset_mint: quote_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    quote_side.reserves = Reserves {
        live_reserve: u128::from(quote_cash),
        cash_reserve: quote_cash,
        ..Reserves::default()
    };
    let srlp_supply = base_cash.min(quote_cash).max(1);
    base_side.shares.srlp_supply = srlp_supply;
    quote_side.shares.srlp_supply = srlp_supply;
    let mut market = Market {
        oracle: Default::default(),
        income: Default::default(),
        tranches: Default::default(),
        junior_exit_config: Default::default(),
        tranche_policy: Default::default(),
        junior_mint: Pubkey::default(),
        version: MARKET_LAYOUT_VERSION,
        srlp_mint: Pubkey::new_unique(),
        base_side: Box::new(base_side),
        quote_side: Box::new(quote_side),
        config: valid_config(),
        amm: Default::default(),
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_borrow_apr_nad: DEFAULT_BORROW_APR_NAD,
            quote_borrow_apr_nad: DEFAULT_BORROW_APR_NAD,
            ..Debt::default()
        },
        base_hlp_vault: HlpVault::default(),
        quote_hlp_vault: HlpVault::default(),
        risk: Risk::default(),
        params_hash: [0u8; 32],
        initial_liquidity_authority: Pubkey::default(),
        operator: Default::default(),
        protocol_interest_markup_bps: 0,

        parameter_revisions: [0; crate::state::PARAMETER_FAMILY_COUNT],
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
        active_liquidation_session: Pubkey::default(),

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
        fixed_base_principal: 0,
        fixed_quote_principal: 0,
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

fn reserve_pair(market: &Market, asset: MarketAsset) -> (u128, u64) {
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
    side.reserves.live_reserve = side.reserves.live_reserve.checked_add(u128::from(accrued_interest)).unwrap();
    accrued_interest
}

#[test]
fn borrow_preserves_virtual_reserve_as_cash_plus_debt() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    let mut borrow_position = BorrowPosition {
        active_liquidation_session: Pubkey::default(),

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
        fixed_base_principal: 0,
        fixed_quote_principal: 0,
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
fn borrow_uses_net_collateral_exit_credit_for_mutable_transfer_fees() {
    let mut gross_market = invariant_market(1_000_000, 1_000_000);
    let mut gross_position = borrow_position_for_debt(MarketAsset::Base, 250_000);
    gross_market
        .borrow(&mut gross_position, MarketAsset::Base, 100_000, 0, 0)
        .unwrap();

    let mut fee_market = invariant_market(1_000_000, 1_000_000);
    let mut fee_position = borrow_position_for_debt(MarketAsset::Base, 250_000);
    let err = fee_market
        .borrow_with_collateral_fee(
            &mut fee_position,
            MarketAsset::Base,
            100_000,
            crate::transitions::LeverageCollateralFee::new(Some(
                spl_token_2022::extension::transfer_fee::TransferFee {
                    epoch: 0_u64.into(),
                    maximum_fee: u64::MAX.into(),
                    transfer_fee_basis_points: 10_000_u16.into(),
                },
            )),
            0,
            0,
        )
        .unwrap_err();
    assert_eq!(err, anchor_lang::prelude::error!(ErrorCode::InsufficientMarketHealth));
}

#[test]
fn transfer_fee_discounts_health_contributions_stored_before_it() {
    let fee =
        crate::transitions::LeverageCollateralFee::new(Some(spl_token_2022::extension::transfer_fee::TransferFee {
            epoch: 0_u64.into(),
            maximum_fee: u64::MAX.into(),
            transfer_fee_basis_points: 7_500_u16.into(),
        }));
    let seeded = || {
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.config.borrow_market_health_floor_bps = 11_000;
        // Alice's contribution is stored while the collateral is fee-free.
        let mut alice = borrow_position_for_debt(MarketAsset::Base, 300_000);
        market.borrow(&mut alice, MarketAsset::Base, 100_000, 0, 0).unwrap();
        market
    };

    let mut fee_free = seeded();
    let mut bob = borrow_position_for_debt(MarketAsset::Base, 1_000_000);
    fee_free.borrow(&mut bob, MarketAsset::Base, 20_000, 0, 0).unwrap();

    // Once the fee applies, Alice's untouched gross contribution is read at
    // its exit value, so the market floor rejects the same draw.
    let mut fee_market = seeded();
    let mut bob = borrow_position_for_debt(MarketAsset::Base, 1_000_000);
    let err = fee_market
        .borrow_with_collateral_fee(&mut bob, MarketAsset::Base, 20_000, fee, 0, 0)
        .unwrap_err();
    assert_eq!(err, anchor_lang::prelude::error!(ErrorCode::InsufficientMarketHealth));
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
            borrow_position_for_debt(borrow_asset, collateral_amount.max(1).try_into().unwrap());
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
            borrow_position_for_debt(repay_asset, collateral_amount.max(1).try_into().unwrap());
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
        prop_assert_eq!(live_after, live_before - u128::from(live_debit));
        prop_assert_eq!(cash_after, cash_before + principal_paid);
        prop_assert!(receipt.interest_paid <= repay_credit);
        prop_assert!(repay_credit <= max_repay_credit);
        prop_assert!(debt_reduction.abs_diff(u128::from(repay_credit)) <= 1);
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
    assert_eq!(live_after, live_before - u128::from(aggregate_debt_reduction - principal_paid));
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
fn partial_repay_keeps_position_principal_clearable_after_another_borrower_exits() {
    use crate::transitions::flash_liquidation::LiquidationPositionRef;

    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = invariant_market(1_000_000, 1_000_000);
        set_borrow_index(&mut market, asset, u128::from(NAD) * 3 / 2);
        let mut first = borrow_position_for_debt(asset, 100_000);
        let mut second = borrow_position_for_debt(asset, 100_000);
        market.borrow(&mut first, asset, 12, 0, 0).unwrap();
        market.borrow(&mut second, asset, 1, 0, 0).unwrap();
        assert_eq!((first.debt_shares(asset), second.debt_shares(asset)), (8, 1));

        let receipt = market.repay(&mut first, asset, 1).unwrap();
        assert_eq!((receipt.cash_repaid, receipt.debt_delta), (1, -2));
        assert_eq!((first.debt_shares(asset), first.principal(asset)), (7, 10));
        assert_eq!(market.public_unpaid_interest(asset).unwrap(), 1);
        market.assert_market_invariants().unwrap();

        let receipt = market.repay(&mut second, asset, 2).unwrap();
        assert_eq!((receipt.cash_repaid, receipt.interest_paid), (2, 1));
        assert_eq!((second.debt_shares(asset), second.principal(asset)), (0, 0));
        let allocation = market
            .liquidation_debt_allocation(&LiquidationPositionRef::Borrow(&first), asset, first.debt_shares(asset))
            .unwrap();
        assert_eq!((allocation.debt, allocation.principal), (10, 10));

        market.repay(&mut first, asset, 10).unwrap();
        assert_eq!((first.debt_shares(asset), first.principal(asset)), (0, 0));
        let aggregate_principal = match asset {
            MarketAsset::Base => market.debt.fixed_base_principal,
            MarketAsset::Quote => market.debt.fixed_quote_principal,
        };
        assert_eq!(aggregate_principal, 0);
        assert_eq!(market.public_unpaid_interest(asset).unwrap(), 0);
        market.assert_market_invariants().unwrap();
    }
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
fn borrower_risk_valuation_uses_curve_depth_ema_cap() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.risk = Risk {
        base_price_ema_nad: NAD,
        quote_price_ema_nad: NAD,
        directional_base_price_ema_nad: NAD,
        directional_quote_price_ema_nad: NAD,
        curve_depth_ema_nad: zero_decimal_nad(100_000),
        ..Risk::default()
    };

    let value = market
        .collateral_value_nad(MarketAsset::Base, 50_000, &market.risk)
        .unwrap();
    let expected = crate::transitions::amm::cpmm_amount_out_nad(
        zero_decimal_nad(100_000),
        zero_decimal_nad(100_000),
        zero_decimal_nad(50_000),
    )
    .unwrap();
    let live_depth_value = crate::transitions::amm::cpmm_amount_out_nad(
        zero_decimal_nad(1_000_000),
        zero_decimal_nad(1_000_000),
        zero_decimal_nad(50_000),
    )
    .unwrap();

    assert!(value > 0);
    assert_eq!(value, expected);
    assert!(value < live_depth_value);
}

#[test]
fn borrowing_uses_only_full_range_tail_of_concentrated_market() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.amm.peak_amplification_nad = 4 * NAD;
    market.config.amm.core_half_width_bps = 1_000;
    market.config.amm.fade_width_bps = 1_000;
    *market.amm = AmmState::default();
    market.risk = Risk::default();
    market.prepare_amm_for_swap(0).unwrap();
    market.refresh_risk().unwrap();

    let collateral_amount = 100_000;
    let collateral_amount_nad = zero_decimal_nad(collateral_amount as u128);
    let cache = market.amm.concentrated_curve_cache;
    let current_depth = cache.tail_liquidity + cache.concentrated_liquidity;
    let risk = Risk {
        base_price_ema_nad: NAD,
        quote_price_ema_nad: NAD,
        directional_base_price_ema_nad: NAD,
        directional_quote_price_ema_nad: NAD,
        observed_curve_depth_nad: current_depth,
        curve_depth_ema_nad: current_depth,
        ..Risk::default()
    };
    let borrow_value = market
        .collateral_value_nad(MarketAsset::Base, collateral_amount, &risk)
        .unwrap();
    let cpmm = ConcentratedCurveGeometry::cpmm();
    let cpmm_point = cpmm.point_at_price_nad(NAD as u128, cache.tail_liquidity).unwrap();
    let tail_only_value = cpmm
        .quote_exact_in(
            cpmm_point,
            collateral_amount_nad,
            ConcentratedCurveDirection::BaseToQuote,
        )
        .unwrap()
        .amount_out;
    let concentrated = cache.geometry().unwrap();
    let concentrated_point = cache.center_point(NAD).unwrap();
    let live_trade_value = concentrated
        .quote_exact_in(
            concentrated_point,
            collateral_amount_nad,
            ConcentratedCurveDirection::BaseToQuote,
        )
        .unwrap()
        .amount_out;

    assert_eq!(borrow_value, tail_only_value);
    assert!(borrow_value < live_trade_value);
}

#[test]
fn reconstructed_risk_curve_caps_depth_by_sudden_current_drawdown() {
    let curve_depth_ema_nad = zero_decimal_nad(1_000_000);
    let high_risk = Risk {
        base_price_ema_nad: NAD,
        quote_price_ema_nad: NAD,
        directional_base_price_ema_nad: NAD,
        directional_quote_price_ema_nad: NAD,
        observed_curve_depth_nad: curve_depth_ema_nad,
        curve_depth_ema_nad,
        ..Risk::default()
    };
    let low_risk = Risk {
        observed_curve_depth_nad: curve_depth_ema_nad / 10,
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
fn daily_borrow_bucket_uses_pessimistic_depth_at_current_spot_ratio() {
    let mut market = invariant_market(4_000_000, 1_000_000);
    market.risk.curve_depth_ema_nad = zero_decimal_nad(1_000_000);

    let (base_depth, quote_depth) = market.pessimistic_borrow_reserve_depths(&market.risk).unwrap();
    assert_eq!(
        market.daily_limit_for_side(MarketAsset::Base, 2_000).unwrap(),
        base_depth / 5
    );
    assert_eq!(
        market.daily_limit_for_side(MarketAsset::Quote, 2_000).unwrap(),
        quote_depth / 5
    );
}

#[test]
fn daily_borrow_bucket_track_post_swap_reserve_ratio() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.risk.curve_depth_ema_nad = zero_decimal_nad(1_000_000);
    let amount_in = 250_000;
    let amount_out = crate::transitions::amm::cpmm_amount_out(1_000_000, 1_000_000, amount_in).unwrap();
    market.base_side.reserves.live_reserve += u128::from(amount_in);
    market.quote_side.reserves.live_reserve -= u128::from(amount_out);

    assert_eq!(market.base_side.reserves.live_reserve, 1_250_000);
    assert_eq!(market.quote_side.reserves.live_reserve, 800_000);
    let (base_depth, quote_depth) = market.pessimistic_borrow_reserve_depths(&market.risk).unwrap();
    assert_eq!(
        market.daily_limit_for_side(MarketAsset::Base, 1_000).unwrap(),
        base_depth / 10
    );
    assert_eq!(
        market.daily_limit_for_side(MarketAsset::Quote, 1_000).unwrap(),
        quote_depth / 10
    );
}

#[test]
fn daily_borrow_bucket_uses_live_depth_when_depth_ema_is_empty_or_above_spot() {
    let mut market = invariant_market(800_000, 1_200_000);

    assert_eq!(market.daily_limit_for_side(MarketAsset::Base, 2_500).unwrap(), 200_000);
    assert!(
        market
            .daily_limit_for_side(MarketAsset::Quote, 2_500)
            .unwrap()
            .abs_diff(300_000)
            <= 1
    );

    market.risk.curve_depth_ema_nad = zero_decimal_nad(2_000_000);
    assert_eq!(market.daily_limit_for_side(MarketAsset::Base, 2_500).unwrap(), 200_000);
    assert!(
        market
            .daily_limit_for_side(MarketAsset::Quote, 2_500)
            .unwrap()
            .abs_diff(300_000)
            <= 1
    );
}

#[test]
fn daily_borrow_bucket_follows_depth_drawdown_growth_and_proportional_liquidity() {
    let mut market = invariant_market(2_000_000, 2_000_000);
    market.risk.curve_depth_ema_nad = zero_decimal_nad(1_000_000);

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
    *market.amm = AmmState::default();
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

/// Six-decimal markets holding a trillion whole base tokens against a million
/// quote tokens, the shape of a large memecoin pool.
fn memecoin_market() -> Market {
    let mut market = invariant_market(1_000_000_000_000_000_000, 1_000_000_000_000);
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    *market.amm = AmmState::default();
    market.risk = Risk::default();
    market.prepare_amm_for_swap(0).unwrap();
    market.refresh_risk().unwrap();
    market
}

#[test]
fn global_health_contribution_keeps_collateral_when_its_cap_exceeds_raw_atoms() {
    // A million quote tokens of debt against base collateral marked at 6e-8:
    // the 150% cap is 2.5e19 base atoms, beyond u64, so the position's own
    // collateral binds. Converting the cap to atoms before the min reverted
    // every deposit, repayment and withdrawal of such a position.
    let market = memecoin_market();
    let risk = Risk {
        base_price_ema_nad: 60,
        directional_base_price_ema_nad: 60,
        ..market.risk
    };
    let collateral = 10_000_000_000_000_000_000;
    assert_eq!(
        market
            .debt_capped_global_health_contribution(MarketAsset::Quote, 1_000_000_000_000, collateral, &risk)
            .unwrap(),
        collateral
    );
}

#[test]
fn borrow_depth_keeps_the_reserve_when_virtual_depth_exceeds_raw_atoms() {
    // A base mark a thousand times below spot puts the pessimistic virtual
    // base reserve near 3.2e19 atoms, beyond u64. The actual reserve binds;
    // converting first reverted every borrow's daily-limit check.
    let mut market = memecoin_market();
    let spot = market.risk.base_price_ema_nad;
    market.risk.base_price_ema_nad = spot / 1_000;
    market.risk.directional_base_price_ema_nad = spot / 1_000;
    let (base_depth, quote_depth) = market.pessimistic_borrow_reserve_depths(&market.risk).unwrap();
    assert_eq!(base_depth, market.curve_reserve(MarketAsset::Base).unwrap());
    assert!(quote_depth > 0);
    assert!(market.daily_limit_for_side(MarketAsset::Base, DEFAULT_DAILY_BORROW_BPS).unwrap() > 0);
}

#[test]
fn repayment_survives_a_collateral_mark_collapse_past_raw_atoms() {
    // A 100,000-quote-token loan against base collateral. When the base mark
    // collapses to 3e-9, the 150% contribution cap on the 60,000 tokens left
    // after a partial repayment reaches 3e19 base atoms, beyond u64; partial
    // and full repayment must still settle.
    let mut market = memecoin_market();
    let mut position = BorrowPosition::default();
    position.clear_liquidation_auction();
    market
        .deposit_collateral(&mut position, MarketAsset::Base, 400_000_000_000_000_000)
        .unwrap();
    market.borrow(&mut position, MarketAsset::Quote, 100_000_000_000, 0, 0).unwrap();
    market.risk.base_price_ema_nad = 3;
    market.risk.directional_base_price_ema_nad = 3;
    market.repay(&mut position, MarketAsset::Quote, 40_000_000_000).unwrap();
    let rest = u64::try_from(position.fixed_quote_debt(&market.debt).unwrap()).unwrap();
    market.repay(&mut position, MarketAsset::Quote, rest).unwrap();
    assert_eq!(position.fixed_quote_debt(&market.debt).unwrap(), 0);
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
        curve_depth_ema_nad: zero_decimal_nad(1_000_000),
        ..Risk::default()
    };

    let health = market.market_health().unwrap();

    assert!(health.base_debt_health_bps <= 15_000);
    assert!(health.effective_base_debt_nad > 0);
}

#[test]
fn recognized_aggregate_collateral_reduces_effective_existing_debt_on_shadow_cpmm() {
    let market = invariant_market(1_000_000, 1_000_000);
    let nad = NAD as u128;
    let existing_debt_nad = 50_000 * nad;
    let projected_debt_nad = 100_000 * nad;
    let reserve_nad = 1_000_000 * nad;

    let (without_recognition, health_without) = market
        .global_side_health_with_virtual_reserves(
            MarketAsset::Base,
            existing_debt_nad,
            projected_debt_nad,
            0,
            &market.risk,
            reserve_nad,
            reserve_nad,
        )
        .unwrap();
    let (with_recognition, health_with) = market
        .global_side_health_with_virtual_reserves(
            MarketAsset::Base,
            existing_debt_nad,
            projected_debt_nad,
            150_000,
            &market.risk,
            reserve_nad,
            reserve_nad,
        )
        .unwrap();

    assert_eq!(without_recognition, existing_debt_nad);
    assert_eq!(health_without, 0);
    assert!(with_recognition < without_recognition);
    assert!(health_with >= market.config.borrow_market_health_floor_bps as u64);
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
        zero_decimal_nad(75_000)
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
fn opposite_collateral_fee_blocks_borrow_below_global_health_floor() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.borrow_market_health_floor_bps = 11_000;
    let mut alice = borrow_position_for_debt(MarketAsset::Quote, 300_000);
    market.borrow(&mut alice, MarketAsset::Quote, 100_000, 0, 0).unwrap();
    let mut bob = borrow_position_for_debt(MarketAsset::Base, 75_000);
    let fee = LeverageCollateralFee::new(Some(spl_token_2022::extension::transfer_fee::TransferFee {
        epoch: 0_u64.into(),
        maximum_fee: u64::MAX.into(),
        transfer_fee_basis_points: 7_500_u16.into(),
    }));
    let fees = LendingCollateralFees {
        base: fee,
        ..Default::default()
    };
    assert!(market.market_health().unwrap().quote_debt_health_bps >= 11_000);
    assert!(
        market
            .market_health_from_risk_with_fees(&market.risk, fees)
            .unwrap()
            .quote_debt_health_bps
            < 11_000
    );
    assert_eq!(
        market
            .borrow_with_market_fees(&mut bob, MarketAsset::Base, 5_000, fees, 0, 0)
            .unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::InsufficientMarketHealth)
    );
    market.borrow(&mut bob, MarketAsset::Base, 5_000, 0, 0).unwrap();
}

#[test]
fn withdrawal_uses_stored_terms_without_enforcing_global_floor() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.borrow_market_health_floor_bps = 11_000;
    let mut position = borrow_position_for_debt(MarketAsset::Base, 250_000);
    market.borrow(&mut position, MarketAsset::Base, 100_000, 0, 0).unwrap();
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
    market.borrow(&mut position, MarketAsset::Base, 100_000, 0, 0).unwrap();
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
    market.borrow(&mut alice, MarketAsset::Base, 100_000, 0, 0).unwrap();
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
    *market.amm = AmmState::default();
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
    fn pessimistic_depth_and_daily_limit_never_exceed_live_inventory(
        base in 1_000_u64..1_000_000_000,
        quote in 1_000_u64..1_000_000_000,
        depth_scale_bps in 1_u128..20_001,
        limit_bps in 0_u16..=BPS_DENOMINATOR,
    ) {
        let mut market = invariant_market(base, quote);
        let spot_q = market
            .amm
            .concentrated_curve_cache
            .tail_liquidity
            .checked_add(market.amm.concentrated_curve_cache.concentrated_liquidity)
            .unwrap();
        market.risk.curve_depth_ema_nad = (spot_q / BPS_DENOMINATOR as u128)
            .checked_mul(depth_scale_bps)
            .unwrap();

        let (base_depth, quote_depth) = market
            .pessimistic_borrow_reserve_depths(&market.risk)
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
    market.base_side.shares.srlp_supply = 1_010;
    market.quote_side.shares.srlp_supply = 1_010;
    *market.amm = AmmState::default();
    market.prepare_amm_for_swap(0).unwrap();
    market.debt.base_borrow_index_nad = (NAD as u128) * 11 / 10;
    market.debt.fixed_base_shares = 100;
    market.debt.fixed_base_principal = 100;
    let mut borrow_position = BorrowPosition {
        active_liquidation_session: Pubkey::default(),

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
        fixed_base_principal: 100,
        fixed_quote_principal: 0,
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
        .execute_parameter_updates(&[MarketParameterUpdate::Fee(fee)], 1)
        .unwrap();
    assert_eq!(market.config.fee_profile(), fee);
    assert_eq!(market.config.max_daily_borrow_bps, before_fee.max_daily_borrow_bps);
    assert_eq!(market.parameter_revisions, [1, 0, 0, 0, 0, 0, 0]);

    let irm = IrmConfig {
        lower_signal_nad: 60_000_000,
        upper_signal_nad: 75_000_000,
        half_life_ms: 12 * MS_PER_DAY,
        ..IrmConfig::default()
    };
    market
        .execute_parameter_updates(&[MarketParameterUpdate::Irm(irm.into())], 2)
        .unwrap();
    assert_eq!(market.config.irm, irm);
    assert_eq!(market.config.fee_profile(), fee);
    assert_eq!(market.parameter_revisions, [1, 0, 1, 0, 0, 0, 0]);

    market
        .execute_parameter_updates(
            &[MarketParameterUpdate::EmaHalfLives {
                price_ms: 120_000,
                directional_price_ms: 180_000,
                curve_depth_ms: 240_000,
                center_price_ms: 300_000,
            }],
            3,
        )
        .unwrap();
    assert_eq!(market.config.ema_half_life_ms, 120_000);
    assert_eq!(market.config.directional_ema_half_life_ms, 180_000);
    assert_eq!(market.config.curve_depth_ema_half_life_ms, 240_000);
    assert_eq!(market.config.amm.center_ema_half_life_ms, 300_000);
    assert_eq!(market.parameter_revisions, [1, 0, 1, 1, 0, 0, 0]);

    market
        .execute_parameter_updates(
            &[MarketParameterUpdate::DailyBorrowLimit {
                max_daily_borrow_bps: 3_000,
            }],
            4,
        )
        .unwrap();
    assert_eq!(market.config.max_daily_borrow_bps, 3_000);
    assert_eq!(market.parameter_revisions, [1, 0, 1, 1, 1, 0, 0]);

    market
        .execute_parameter_updates(
            &[MarketParameterUpdate::CenterController {
                adjustment_step_nad: NAD / 500,
            }],
            5,
        )
        .unwrap();
    assert_eq!(market.config.amm.adjustment_step_nad, NAD / 500);
    assert_eq!(market.parameter_revisions, [1, 0, 1, 1, 1, 1, 0]);
}

#[test]
fn fee_compounding_rate_is_configurable_from_zero_through_one_hundred_percent() {
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
fn concentration_execution_schedules_guarded_intermediate_and_final_shapes() {
    let mut market = controller_market(1_000_000, 1_000_000);
    market.finalize_amm_transition_and_observe_risk(1).unwrap();
    let old_price = market.current_concentrated_spot_price_nad().unwrap();
    let old_value = market.funded_reserve_value().unwrap();
    market
        .execute_parameter_updates(
            &[MarketParameterUpdate::Concentration {
                peak_amplification_nad: 4 * NAD,
                core_half_width_bps: 100,
                fade_width_bps: 400,
                ramp_duration_slots: MIN_CONCENTRATION_RAMP_SLOTS,
            }],
            2,
        )
        .unwrap();
    assert_eq!(market.config.amm.peak_amplification_nad, NAD);
    assert_eq!(market.amm.concentration_ramp.target.peak_amplification_nad, 4 * NAD);
    assert!(market
        .advance_concentration_ramp(2 + MIN_CONCENTRATION_RAMP_SLOTS / 2)
        .unwrap());
    assert_eq!(market.config.amm.peak_amplification_nad, 5 * NAD / 2);
    assert!(market.amm.concentration_ramp.active);
    let intermediate = market.try_to_vec().unwrap();
    assert!(!market.advance_concentration_ramp(2 + MIN_CONCENTRATION_RAMP_SLOTS / 2).unwrap());
    assert_eq!(market.try_to_vec().unwrap(), intermediate);
    assert!(
        market
            .current_concentrated_spot_price_nad()
            .unwrap()
            .unwrap()
            .abs_diff(old_price.unwrap())
            <= 1
    );
    assert_eq!(market.funded_reserve_value().unwrap(), old_value);
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
    assert!(market
        .advance_concentration_ramp(2 + MIN_CONCENTRATION_RAMP_SLOTS)
        .unwrap());
    assert_eq!(market.config.amm.peak_amplification_nad, 4 * NAD);
    assert!(!market.amm.concentration_ramp.active);
    assert_eq!(market.parameter_revisions, [0, 1, 0, 0, 0, 0, 0]);
}

#[test]
fn concentration_requires_the_minimum_base_fee() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.amm.adjustment_step_nad = NAD / 1_000;
    market.finalize_amm_transition_and_observe_risk(1).unwrap();
    assert_eq!(market.config.swap_fee_bps, 0);
    let concentration = MarketParameterUpdate::Concentration {
        peak_amplification_nad: 4 * NAD,
        core_half_width_bps: 100,
        fade_width_bps: 400,
        ramp_duration_slots: MIN_CONCENTRATION_RAMP_SLOTS,
    };

    // The proposal is checked against its target, so a fee-free market cannot
    // schedule a concentrated curve.
    let before = market.try_to_vec().unwrap();
    assert_eq!(
        market
            .execute_parameter_updates(core::slice::from_ref(&concentration), 2)
            .unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::ConcentratedSwapFeeBelowMinimum)
    );
    assert_eq!(market.try_to_vec().unwrap(), before);

    // Raising the base fee in the same proposal admits it.
    let mut minimum = market.config.fee_profile();
    minimum.base_fee_bps = MIN_CONCENTRATED_SWAP_FEE_BPS;
    market
        .execute_parameter_updates(&[MarketParameterUpdate::Fee(minimum), concentration], 2)
        .unwrap();
    assert!(market
        .advance_concentration_ramp(2 + MIN_CONCENTRATION_RAMP_SLOTS)
        .unwrap());
    assert_eq!(market.config.amm.peak_amplification_nad, 4 * NAD);

    // Once concentrated, the fee cannot return below the minimum.
    let mut fee_free = market.config.fee_profile();
    fee_free.base_fee_bps = 0;
    assert_eq!(
        market
            .validate_parameter_updates(&[MarketParameterUpdate::Fee(fee_free)])
            .unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::ConcentratedSwapFeeBelowMinimum)
    );
}

#[test]
fn returning_to_cpmm_releases_retained_principal_only_at_a_guarded_step() {
    let mut market = controller_market(1_000_000, 1_000_000);
    market.finalize_amm_transition_and_observe_risk(1).unwrap();
    let update = |amp, core, fade| MarketParameterUpdate::Concentration {
        peak_amplification_nad: amp,
        core_half_width_bps: core,
        fade_width_bps: fade,
        ramp_duration_slots: MIN_CONCENTRATION_RAMP_SLOTS,
    };
    market
        .execute_parameter_updates(&[update(2 * NAD, 100, 400)], 2)
        .unwrap();
    let end = 2 + MIN_CONCENTRATION_RAMP_SLOTS;
    assert!(market.advance_concentration_ramp(end).unwrap());
    let before = market.try_to_vec().unwrap();
    assert!(market
        .execute_parameter_updates(
            &[MarketParameterUpdate::CenterController { adjustment_step_nad: 0 }],
            end + 1
        )
        .is_err());
    assert_eq!(market.try_to_vec().unwrap(), before);
    let protected = 1_000;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        market.credit_protected_recenter_reserve(asset, protected).unwrap();
    }
    let base = market.base_side.reserves.live_reserve;
    let quote = market.quote_side.reserves.live_reserve;
    market.execute_parameter_updates(&[update(NAD, 0, 0)], end + 1).unwrap();
    assert_eq!(market.base_side.reserves.protected_recenter_reserve, protected);
    assert!(market
        .advance_concentration_ramp(end + 1 + MIN_CONCENTRATION_RAMP_SLOTS)
        .unwrap());
    assert_eq!(market.config.amm.peak_amplification_nad, NAD);
    assert_eq!(market.base_side.reserves.protected_recenter_reserve, 0);
    assert_eq!(market.quote_side.reserves.protected_recenter_reserve, 0);
    assert_eq!(market.base_side.reserves.live_reserve, base + u128::from(protected));
    assert_eq!(market.quote_side.reserves.live_reserve, quote + u128::from(protected));
    assert_eq!(market.amm.spendable_protected_profit_nad(), 0);
    market.assert_market_invariants().unwrap();
}

#[test]
fn active_or_residual_hlp_allows_scheduling_and_guarded_concentration_steps() {
    let update = MarketParameterUpdate::Concentration {
        ramp_duration_slots: MIN_CONCENTRATION_RAMP_SLOTS,
        peak_amplification_nad: 4 * NAD,
        core_half_width_bps: 100,
        fade_width_bps: 400,
    };

    for mut market in [
        {
            let mut active = controller_market(1_000_000, 1_000_000);
            active.finalize_amm_transition_and_observe_risk(0).unwrap();
            active.base_hlp_vault.hlp_supply = 1;
            active
        },
        {
            let mut residual = controller_market(1_000_000, 1_000_000);
            residual.finalize_amm_transition_and_observe_risk(0).unwrap();
            residual.quote_hlp_vault.residual_exposure = 1;
            residual
        },
    ] {
        market
            .execute_parameter_updates(core::slice::from_ref(&update), 1)
            .unwrap();
        assert_eq!(market.amm.concentrated_curve_cache.peak_amplification_nad, NAD);
        assert!(market
            .advance_concentration_ramp(1 + MIN_CONCENTRATION_RAMP_SLOTS)
            .unwrap());
        assert_eq!(market.amm.concentrated_curve_cache.peak_amplification_nad, 4 * NAD);
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
        .execute_parameter_updates(
            &[MarketParameterUpdate::DailyBorrowLimit {
                max_daily_borrow_bps: 1_000,
            }],
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
        .execute_parameter_updates(
            &[MarketParameterUpdate::DailyBorrowLimit {
                max_daily_borrow_bps: 3_000,
            }],
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
fn one_proposal_applies_every_family_and_advances_each_revision() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    let irm = IrmConfig {
        lower_signal_nad: 60_000_000,
        upper_signal_nad: 75_000_000,
        half_life_ms: 12 * MS_PER_DAY,
        ..IrmConfig::default()
    };
    let updates = [
        MarketParameterUpdate::Irm(irm.into()),
        MarketParameterUpdate::DailyBorrowLimit {
            max_daily_borrow_bps: 3_000,
        },
        MarketParameterUpdate::Tranche {
            junior_income_share_bps: 4_500,
        },
    ];
    let before = market.config;

    market.execute_parameter_updates(&updates, 1).unwrap();

    assert_eq!(market.config.irm, irm);
    assert_eq!(market.config.max_daily_borrow_bps, 3_000);
    assert_eq!(market.config.fee_profile(), before.fee_profile());
    assert_eq!(market.config.amm, before.amm);
    assert_eq!(market.parameter_revisions, [0, 0, 1, 0, 1, 0, 1]);
    market.assert_market_invariants().unwrap();
}

#[test]
fn update_sets_reject_repeated_unordered_or_unchanged_families() {
    let market = invariant_market(1_000_000, 1_000_000);
    let borrow_limit = |bps| MarketParameterUpdate::DailyBorrowLimit {
        max_daily_borrow_bps: bps,
    };
    let irm = MarketParameterUpdate::Irm(
        IrmConfig {
            lower_signal_nad: 60_000_000,
            upper_signal_nad: 75_000_000,
            half_life_ms: 12 * MS_PER_DAY,
            ..IrmConfig::default()
        }
        .into(),
    );
    let not_canonical = anchor_lang::prelude::error!(ErrorCode::ParameterUpdatesNotCanonical);

    assert_eq!(market.validate_parameter_updates(&[]).unwrap_err(), not_canonical);
    assert_eq!(
        market
            .validate_parameter_updates(&[borrow_limit(3_000), borrow_limit(4_000)])
            .unwrap_err(),
        not_canonical
    );
    assert_eq!(
        market
            .validate_parameter_updates(&[borrow_limit(3_000), irm.clone()])
            .unwrap_err(),
        not_canonical
    );
    // Every family in a set must change its own values; one no-op family
    // rejects the whole proposal.
    assert_eq!(
        market
            .validate_parameter_updates(&[irm.clone(), borrow_limit(market.config.max_daily_borrow_bps)])
            .unwrap_err(),
        anchor_lang::prelude::error!(ErrorCode::ParameterUpdateNotMeaningful)
    );
    // One out-of-bounds family rejects the whole proposal.
    assert!(market
        .validate_parameter_updates(&[irm.clone(), borrow_limit(MAX_DAILY_BORROW_BPS + 1)])
        .is_err());
    market.validate_parameter_updates(&[irm, borrow_limit(3_000)]).unwrap();
}

#[test]
fn a_failing_multi_family_execution_leaves_every_family_untouched() {
    let mut market = invariant_market(1_000, 1_000);
    market.debt.fixed_base_shares = 800;
    market.debt.fixed_base_principal = 800;
    market.base_side.reserves.cash_reserve = 200;
    let before_market = market.try_to_vec().unwrap();

    let error = market
        .execute_parameter_updates(
            &[
                MarketParameterUpdate::DailyBorrowLimit {
                    max_daily_borrow_bps: 1_000,
                },
                MarketParameterUpdate::Tranche {
                    junior_income_share_bps: 4_500,
                },
            ],
            MS_PER_YEAR / TARGET_MS_PER_SLOT,
        )
        .unwrap_err();

    assert_eq!(error, anchor_lang::prelude::error!(ErrorCode::UtilizationGuardExceeded));
    assert_eq!(market.try_to_vec().unwrap(), before_market);
}

#[test]
fn failed_configuration_restores_live_hlp_funding_checkpoints() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.deposit_single_sided(MarketAsset::Base, 20_000, 0).unwrap();
    market.deposit_single_sided(MarketAsset::Quote, 20_000, 0).unwrap();
    let principal = market.base_side.reserves.cash_reserve * 9 / 10;
    market.debt.fixed_base_principal = principal;
    market.debt.fixed_base_shares = Debt::debt_to_shares(principal, market.debt.base_borrow_index_nad).unwrap();
    market.base_side.reserves.cash_reserve -= principal;
    let before = market.try_to_vec().unwrap();
    let result = market.execute_parameter_updates(
        &[MarketParameterUpdate::DailyBorrowLimit { max_daily_borrow_bps: 1_000 }],
        100,
    );
    assert!(result.is_err());
    assert_eq!(market.try_to_vec().unwrap(), before);
}

#[test]
fn every_family_closes_its_windows_before_any_family_moves() {
    let half_day_slot = MS_PER_DAY / TARGET_MS_PER_SLOT / 2;
    let borrow_limit = MarketParameterUpdate::DailyBorrowLimit {
        max_daily_borrow_bps: 3_000,
    };
    let reshaped = || {
        let mut market = controller_market(1_000_000, 1_000_000);
        market.finalize_amm_transition_and_observe_risk(0).unwrap();
        market.base_side.daily_borrow_bucket.borrowed_bucket = 200_000;
        market.quote_side.daily_borrow_bucket.borrowed_bucket = 150_000;
        market
    };

    let mut alone = reshaped();
    alone
        .execute_parameter_updates(core::slice::from_ref(&borrow_limit), half_day_slot)
        .unwrap();

    // The curve family runs first and rebuilds the risk depths, but the borrow
    // buckets still refill at the old rate against the old depths.
    let mut together = reshaped();
    together
        .execute_parameter_updates(
            &[
                MarketParameterUpdate::Concentration {
                    ramp_duration_slots: MIN_CONCENTRATION_RAMP_SLOTS,
                    peak_amplification_nad: 4 * NAD,
                    core_half_width_bps: 100,
                    fade_width_bps: 400,
                },
                borrow_limit,
            ],
            half_day_slot,
        )
        .unwrap();

    for (together, alone) in [
        (
            together.base_side.daily_borrow_bucket,
            alone.base_side.daily_borrow_bucket,
        ),
        (
            together.quote_side.daily_borrow_bucket,
            alone.quote_side.daily_borrow_bucket,
        ),
    ] {
        assert_eq!(together.try_to_vec().unwrap(), alone.try_to_vec().unwrap());
        assert!(together.borrowed_bucket > 0 && together.last_decay_slot == half_day_slot);
    }
    assert_eq!(together.amm.concentrated_curve_cache.peak_amplification_nad, NAD);
    assert!(together
        .advance_concentration_ramp(half_day_slot + MIN_CONCENTRATION_RAMP_SLOTS)
        .unwrap());
    assert_eq!(together.amm.concentrated_curve_cache.peak_amplification_nad, 4 * NAD);
    assert_eq!(together.parameter_revisions, [0, 1, 0, 0, 1, 0, 0]);
}

#[test]
fn curve_and_center_controller_validate_together_and_cannot_disable_centering() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.swap_fee_bps = MIN_CONCENTRATED_SWAP_FEE_BPS;
    market
        .execute_parameter_updates(
            &[
                MarketParameterUpdate::Concentration {
                    peak_amplification_nad: 2 * NAD,
                    core_half_width_bps: 100,
                    fade_width_bps: 400,
                    ramp_duration_slots: MIN_CONCENTRATION_RAMP_SLOTS,
                },
                MarketParameterUpdate::CenterController {
                    adjustment_step_nad: NAD / 500,
                },
            ],
            1,
        )
        .unwrap();
    assert_eq!(market.config.amm.peak_amplification_nad, NAD);
    assert_eq!(market.config.amm.adjustment_step_nad, NAD / 500);
    assert!(market
        .advance_concentration_ramp(1 + MIN_CONCENTRATION_RAMP_SLOTS)
        .unwrap());
    assert_eq!(market.config.amm.peak_amplification_nad, 2 * NAD);
    assert_eq!(market.parameter_revisions, [0, 1, 0, 0, 0, 1, 0]);
    let before = market.try_to_vec().unwrap();
    assert!(market
        .execute_parameter_updates(
            &[MarketParameterUpdate::CenterController { adjustment_step_nad: 0 }],
            2 + MIN_CONCENTRATION_RAMP_SLOTS
        )
        .is_err());
    assert_eq!(market.try_to_vec().unwrap(), before);
    market.assert_market_invariants().unwrap();
}

#[test]
fn tranche_governance_changes_only_future_income_share() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.base_side.fees.junior.initialized = true;
    market.quote_side.fees.junior.initialized = true;
    market
        .execute_parameter_updates(
            &[MarketParameterUpdate::Tranche {
                junior_income_share_bps: 6_000,
            }],
            1,
        )
        .unwrap();
    assert_eq!(market.tranche_policy.junior_income_share_bps, 6_000);
    assert_eq!(market.base_side.fees.junior.income_share_bps, 6_000);
    assert_eq!(market.quote_side.fees.junior.income_share_bps, 6_000);
    assert_eq!(market.parameter_revisions[ParameterFamily::Tranche.code() as usize], 1);
    for share in [999, 9_001] {
        assert!(market
            .validate_parameter_updates(&[MarketParameterUpdate::Tranche {
                junior_income_share_bps: share
            }])
            .is_err());
    }
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
    market.debt.quote_borrow_apr_nad = 0;
    let half_life_slots = HLP_FUNDING_APR_EMA_HALF_LIFE_MS / TARGET_MS_PER_SLOT;
    market.accrue_interest_to_slot(100 + half_life_slots).unwrap();
    let decayed = market.base_hlp_vault.funding_apr_ema_nad;
    assert!(decayed > 0);
    assert!(decayed < signal);
}

#[test]
fn high_decimal_borrow_partial_repay_and_liquidation_preserve_dust_debt() {
    for (base_decimals, quote_decimals) in [(12, 6), (6, 12), (18, 9), (10, 10)] {
        // Start with a valid fixture, then rebuild the curve at the mint scales.
        let mut market = invariant_market(1_000_000, 1_000_000);
        market.base_side.asset_decimals = base_decimals;
        market.quote_side.asset_decimals = quote_decimals;
        let precision = market.amount_decimals().unwrap();
        let normalized_reserve = 1_000_000_000_000_000_u64;
        market.base_side.reserves.live_reserve = u128::from(normalized_reserve / 10_u64.pow(u32::from(precision - base_decimals)));
        market.base_side.reserves.cash_reserve = market.base_side.reserves.live_reserve.try_into().unwrap();
        market.quote_side.reserves.live_reserve =
            u128::from(normalized_reserve / 10_u64.pow(u32::from(precision - quote_decimals)));
        market.quote_side.reserves.cash_reserve = market.quote_side.reserves.live_reserve.try_into().unwrap();
        *market.amm = AmmState::default();
        market.risk = Risk::default();
        market.prepare_amm_for_swap(0).unwrap();
        market.refresh_risk().unwrap();
        assert_eq!(market.risk.cached_spot_base_price_nad, NAD);
        let debt_asset = if base_decimals >= quote_decimals {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let collateral = market.side(debt_asset.opposite()).reserves.cash_reserve / 10;
        let mut position = borrow_position_for_debt(debt_asset, collateral);
        market.borrow(&mut position, debt_asset, 2_000_000_001, 0, 0).unwrap();
        market.repay(&mut position, debt_asset, 2_000_000_000).unwrap();
        let debt = match debt_asset {
            MarketAsset::Base => position.fixed_base_debt(&market.debt).unwrap(),
            MarketAsset::Quote => position.fixed_quote_debt(&market.debt).unwrap(),
        };
        assert_eq!(debt, 1);
        assert_eq!(
            market.total_fixed_debt_nad(debt_asset).unwrap(),
            market.normalize_amount(1, market.side(debt_asset).asset_decimals).unwrap()
        );
        // An underwater remainder must remain visible to risk checks.
        position.base_collateral = 0;
        position.quote_collateral = 0;
        assert!(market
            .is_position_liquidatable_with_risk(&position, debt_asset, &market.risk)
            .unwrap());
        market.repay(&mut position, debt_asset, 1).unwrap();
        assert_eq!(market.total_fixed_debt_nad(debt_asset).unwrap(), 0);
        assert!(!market
            .is_position_liquidatable_with_risk(&position, debt_asset, &market.risk)
            .unwrap());
    }
}

#[test]
fn high_decimal_launch_fee_buckets_keep_nine_decimal_config_units() {
    for decimals in [9, 12, 18] {
        let mut market = invariant_market(1_000_000_000_000_000_000, 1_000_000_000_000_000_000);
        market.base_side.asset_decimals = decimals;
        market.quote_side.asset_decimals = decimals;
        *market.amm = AmmState::default();
        market.risk = Risk::default();
        market.config.swap_fee_bps = 100;
        market.config.amm.launch_rate_limit_asset = LAUNCH_RATE_LIMIT_ASSET_BASE;
        market.config.amm.launch_rate_limit_reference_nad = NAD;
        market.config.amm.launch_rate_limit_increment_bps = 100;
        market.config.amm.launch_rate_limit_max_fee_bps = 1_000;
        market.config.amm.launch_rate_limit_duration_seconds = 100;
        market.prepare_amm_for_swap(0).unwrap();
        let one_token = 10_u64.pow(u32::from(decimals));
        for (input, fee_bps) in [(one_token, 100_u64), (one_token + 1, 200)] {
            let quote = market
                .preliminary_swap_inputs_for_state_at_time(
                    MarketAsset::Quote,
                    input,
                    0,
                    0,
                    crate::transitions::amm::DynamicFeePreState::default(),
                )
                .unwrap();
            let expected_fee = (u128::from(input) * u128::from(fee_bps) / u128::from(BPS_DENOMINATOR)) as u64;
            assert_eq!(
                quote.amount_in_for_quote,
                input - expected_fee,
                "decimals={decimals}, input={input}"
            );
        }
    }
}

#[test]
fn protection_health_uses_liquidation_terms_and_donations_preserve_them() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    let mut position = borrow_position_for_debt(MarketAsset::Base, 250_000);
    market
        .borrow(&mut position, MarketAsset::Base, 100_000, 8_000, 0)
        .unwrap();
    let cf = position.base_liquidation_cf_bps;
    let before = market
        .borrow_protection_health_bps(&position, MarketAsset::Base)
        .unwrap();
    assert!(before > 10_000);
    let after_fee_increase = market
        .borrow_protection_health_bps_with_credit(&position, MarketAsset::Base, position.quote_collateral / 2)
        .unwrap();
    assert!(after_fee_increase < before);
    assert_eq!(
        market
            .borrow_protection_health_bps_with_credit(&position, MarketAsset::Base, 0)
            .unwrap(),
        0
    );
    market
        .deposit_collateral(&mut position, MarketAsset::Quote, 10_000)
        .unwrap();
    let after = market
        .borrow_protection_health_bps(&position, MarketAsset::Base)
        .unwrap();
    assert!(after > before);
    assert_eq!(position.base_liquidation_cf_bps, cf);
    market.repay(&mut position, MarketAsset::Base, 100_000).unwrap();
    assert_eq!(
        market
            .borrow_protection_health_bps(&position, MarketAsset::Base)
            .unwrap(),
        u64::MAX
    );
}

#[test]
fn unavailable_oracle_allows_principal_and_interest_repayment_without_seizure() {
    use crate::state::{OraclePolicy, RuntimePricing};
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        for with_hlp in [false, true] {
            let mut market = invariant_market(1_000_000, 1_000_000);
            if with_hlp {
                market.deposit_single_sided(MarketAsset::Base, 20_000, 0).unwrap();
                market.deposit_single_sided(MarketAsset::Quote, 20_000, 0).unwrap();
            }
            let mut position = borrow_position_for_debt(asset, 100_000);
            market.borrow(&mut position, asset, 10_000, 0, 0).unwrap();
            market.oracle.policy = OraclePolicy {
                lambda_bps: 5000,
                adjustment_cap_nad: 2 * NAD,
                ..Default::default()
            };
            market.oracle.runtime = RuntimePricing::default();
            let collateral = position.collateral(asset.opposite());
            let risk = market.risk.try_to_vec().unwrap();
            market.accrue_interest_to_slot(1).unwrap();
            market.accrue_interest_to_slot(100_000_000).unwrap();
            let debt = Debt::shares_to_debt(position.debt_shares(asset), market.debt.borrow_index(asset)).unwrap();
            assert!(debt > 10_000);
            assert!(market.borrow(&mut position, asset, 1, 0, 100_000_000).is_err());
            // This zero-decimal fixture can accrue until one debt share costs
            // more than 100 raw units under the dynamic rate controller. Ask
            // the canonical preview for the exact aggregate-floor cash debit
            // that burns at least one share while remaining partial.
            let one_share_ceiling = u64::try_from(market.debt.borrow_index(asset).div_ceil(NAD as u128)).unwrap();
            let partial_repay = market
                .fixed_repayment_for_max(&position, asset, one_share_ceiling)
                .unwrap()
                .cash_repaid;
            market
                .repay_with_finalization_and_fee(
                    &mut position,
                    asset,
                    partial_repay,
                    LeverageCollateralFee::default(),
                    Some(100_000_000),
                )
                .unwrap();
            assert_eq!(position.collateral(asset.opposite()), collateral);
            assert!(position.debt_shares(asset) > 0);
            assert_eq!(market.risk.try_to_vec().unwrap(), risk);
            let remaining = u64::try_from(
                Debt::shares_to_debt(position.debt_shares(asset), market.debt.borrow_index(asset)).unwrap(),
            )
            .unwrap();
            market
                .repay_with_finalization_and_fee(
                    &mut position,
                    asset,
                    remaining,
                    LeverageCollateralFee::default(),
                    Some(100_000_000),
                )
                .unwrap();
            assert_eq!(position.debt_shares(asset), 0);
            assert_eq!(position.collateral(asset.opposite()), collateral);
            market.assert_market_invariants().unwrap();
        }
    }
}

#[test]
fn launch_configuration_is_frozen_at_start_while_runtime_fees_remain_governable() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.start_time = 100;
    let original = market.config.fee_profile();
    let mut variants = vec![original; 11];
    variants[0].launch_fee_start_bps = 50;
    variants[1].launch_fee_duration_seconds = 86_400;
    variants[2].launch_fee_decay_mode = LAUNCH_FEE_DECAY_LINEAR;
    variants[3].launch_market_price_step_bps = 100;
    variants[4].launch_market_number_of_periods = 2;
    variants[5].launch_market_reduction_factor_bps = 100;
    variants[6].launch_rate_limit_asset = LAUNCH_RATE_LIMIT_ASSET_BASE;
    variants[7].launch_rate_limit_reference_nad = NAD;
    variants[8].launch_rate_limit_increment_bps = 10;
    variants[9].launch_rate_limit_max_fee_bps = 100;
    variants[10].launch_rate_limit_duration_seconds = 86_400;
    for variant in variants {
        let updates = [MarketParameterUpdate::Fee(variant)];
        assert!(market
            .assert_launch_settings_unchanged_after_start(&updates, 99)
            .is_ok());
        assert!(market
            .assert_launch_settings_unchanged_after_start(&updates, 100)
            .is_err());
        assert!(market
            .assert_launch_settings_unchanged_after_start(&updates, 1_000_000)
            .is_err());
    }
    let mut runtime = original;
    runtime.base_fee_bps = 100;
    runtime.compounding_fee_bps = 10_000;
    runtime.swap_fee_collect_mode = SWAP_FEE_COLLECT_QUOTE_ONLY;
    assert!(market
        .assert_launch_settings_unchanged_after_start(&[MarketParameterUpdate::Fee(runtime)], 100)
        .is_ok());
}

#[test]
fn queued_launch_change_cannot_cross_launch_boundary_at_execution() {
    let mut market = invariant_market(1_000_000, 1_000_000);
    market.config.start_time = 100;
    let mut fee = market.config.fee_profile();
    fee.launch_fee_start_bps = 50;
    fee.launch_fee_duration_seconds = 86_400;
    fee.launch_fee_decay_mode = LAUNCH_FEE_DECAY_LINEAR;
    let updates = [MarketParameterUpdate::Fee(fee)];
    market.validate_parameter_updates(&updates).unwrap();
    market
        .assert_launch_settings_unchanged_after_start(&updates, 99)
        .unwrap();
    let before_config = market.config;
    let before_revisions = market.parameter_revisions;
    let clock = Clock {
        unix_timestamp: 100,
        slot: 1,
        ..Default::default()
    };
    let error = market
        .execute_parameter_updates_authenticated(&updates, &clock, &[])
        .unwrap_err();
    assert_eq!(error, error!(ErrorCode::LaunchSettingsImmutable));
    assert_eq!(market.config, before_config);
    assert_eq!(market.parameter_revisions, before_revisions);
}

#[test]
fn new_borrower_cannot_reclassify_older_borrowers_principal_or_tranche_loss() {
    use crate::transitions::flash_liquidation::LiquidationPositionRef;
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = invariant_market(1_000_000, 1_000_000);
        let mut alice = borrow_position_for_debt(asset, 10_000);
        market.borrow(&mut alice, asset, 1_000, 0, 0).unwrap();
        let alice_shares = alice.debt_shares(asset);
        set_borrow_index(&mut market, asset, (NAD as u128) * 11 / 10);
        add_accrued_cash_backed_interest_to_live_reserve(&mut market, asset, alice_shares, 1_000);

        let mut bob = borrow_position_for_debt(asset, 10_000);
        market.borrow(&mut bob, asset, 1_100, 0, 0).unwrap();
        let repayment = market.repay(&mut bob, asset, 1_100).unwrap();
        assert_eq!(repayment.interest_paid, 0);
        assert_eq!(bob.principal(asset), 0);
        assert_eq!(alice.principal(asset), 1_000);
        assert_eq!(market.public_unpaid_interest(asset).unwrap(), 100);
        let allocation = market
            .liquidation_debt_allocation(&LiquidationPositionRef::Borrow(&alice), asset, alice_shares)
            .unwrap();
        assert_eq!(allocation.debt, 1_100);
        assert_eq!(allocation.principal, 1_000);
        let loss = crate::LiquidationLossAllocation::quote(allocation.principal, allocation.debt, 900).unwrap();
        assert_eq!(loss.principal_written_off, 100);
        market.assert_virtual_reserve_invariant(asset).unwrap();
    }
}

fn controller_market(base_cash: u64, quote_cash: u64) -> Market {
    let mut market = invariant_market(base_cash, quote_cash);
    market.config.amm.adjustment_step_nad = NAD / 1_000;
    // Concentration targets require the minimum concentrated base fee.
    market.config.swap_fee_bps = MIN_CONCENTRATED_SWAP_FEE_BPS;
    market
}

#[test]
fn same_slot_borrow_and_repay_loops_do_not_exhaust_the_daily_limit() {
    let debt_asset = MarketAsset::Quote;
    let mut market = invariant_market(1_000_000, 1_000_000);
    let mut position = borrow_position_for_debt(debt_asset, 600_000);
    let limit = market
        .daily_limit_for_side(debt_asset, market.config.max_daily_borrow_bps)
        .unwrap();
    let amount = limit / 2;
    assert!(amount > 0);
    // Gross accounting filled the bucket after two loops and refused the third.
    for _ in 0..10 {
        market.borrow(&mut position, debt_asset, amount, 0, 0).unwrap();
        let credit = market
            .fixed_repayment_for_max(&position, debt_asset, amount * 2)
            .unwrap()
            .cash_repaid;
        market.repay(&mut position, debt_asset, credit).unwrap();
        assert_eq!(market.side(debt_asset).daily_borrow_bucket.borrowed_bucket, 0);
    }
    market.borrow(&mut position, debt_asset, limit, 0, 0).unwrap();
    assert!(market.borrow(&mut position, debt_asset, 1, 0, 0).is_err());
    market.assert_market_invariants().unwrap();
}

#[test]
fn collateral_withdrawal_cannot_cross_the_market_health_floor() {
    let debt_asset = MarketAsset::Quote;
    let mut market = invariant_market(1_000_000, 1_000_000);
    let mut position = borrow_position_for_debt(debt_asset, 200_000);
    market.borrow(&mut position, debt_asset, 80_000, 0, 0).unwrap();
    let health = |m: &Market| m.market_health().unwrap().quote_debt_health_bps;
    let before = health(&market);
    // The position's contribution is capped, so only a withdrawal below the
    // cap lowers market health; find the first one the position can make.
    let (withdrawal, after) = (1..=150_u64)
        .map(|i| i * 1_000)
        .find_map(|withdrawal| {
            let mut trial = market.clone();
            trial
                .withdraw_collateral(&mut position.clone(), MarketAsset::Base, withdrawal, 0)
                .ok()?;
            let after = health(&trial);
            (after < before).then_some((withdrawal, after))
        })
        .expect("a permitted withdrawal lowers market health");
    let withdraw_at = |floor: u64| {
        let mut market = market.clone();
        market.config.borrow_market_health_floor_bps = u16::try_from(floor).unwrap();
        market.withdraw_collateral(&mut position.clone(), MarketAsset::Base, withdrawal, 0)
    };
    // From at or above the floor to below it: refused.
    assert!(withdraw_at(before).is_err());
    assert!(withdraw_at((before + after) / 2 + 1).is_err());
    // Health stays at or above the floor: allowed.
    assert!(withdraw_at(after).is_ok());
    // Already below the floor: surplus collateral is not trapped.
    assert!(withdraw_at(before + 1).is_ok());
}

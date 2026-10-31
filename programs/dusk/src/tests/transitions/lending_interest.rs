use super::*;

// Market-level interest checkpoints and debt-accounting invariants.
use crate::{
    constants::{
        INTEREST_INITIAL_RATE_AT_TARGET_NAD, INTEREST_MAX_RATE_AT_TARGET_NAD, INTEREST_MIN_RATE_AT_TARGET_NAD,
        MARKET_LAYOUT_VERSION, MS_PER_YEAR, NAD, TARGET_MS_PER_SLOT,
    },
    state::{Debt, HlpVault, Insurance, MarketConfig, MarketSide, Reserves, Risk},
};

fn slots_for_ms(ms: u64) -> u64 {
    ms / TARGET_MS_PER_SLOT
}

fn test_market(base_cash: u64, quote_cash: u64) -> Market {
    let base_side = MarketSide {
        reserves: Reserves {
            live_reserve: base_cash,
            cash_reserve: base_cash,
            ..Reserves::default()
        },
        ..MarketSide::default()
    };
    let quote_side = MarketSide {
        reserves: Reserves {
            live_reserve: quote_cash,
            cash_reserve: quote_cash,
            ..Reserves::default()
        },
        ..MarketSide::default()
    };
    Market {
        income: Default::default(),
        tranches: Default::default(),
        junior_exit_config: Default::default(),
        junior_mint: Pubkey::default(),
        version: MARKET_LAYOUT_VERSION,
        ylp_mint: Pubkey::new_unique(),
        base_side: Box::new(base_side),
        quote_side: Box::new(quote_side),
        config: MarketConfig::default(),
        amm: Default::default(),
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            quote_rate_at_target_nad: INTEREST_INITIAL_RATE_AT_TARGET_NAD,
            base_last_accrual_slot: 0,
            quote_last_accrual_slot: 0,
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
    }
}

fn configure_active_base_side(market: &mut Market) {
    // At index 1.0: 300 fixed + 200 isolated + 200 hLP funding debt,
    // backed by 300 idle cash. The side is therefore exactly at the 70%
    // utilization target, while the quote side remains debt-free.
    market.debt.fixed_base_shares = 300;
    market.debt.fixed_base_principal = 300;
    market.debt.isolated_base_shares = 200;
    market.debt.isolated_base_principal = 200;
    market.quote_hlp_vault.debt_shares = 200;
    market.quote_hlp_vault.debt_principal = 200;
    market.quote_hlp_vault.base_hlp_live_reserve = 200;
    market.base_side.reserves.live_reserve = 1_000;
}

#[test]
fn accrual_receipt_separates_credit_margin_and_hlp_at_the_shared_index() {
    let mut market = test_market(300, 1_000);
    configure_active_base_side(&mut market);
    market.debt.base_rate_at_target_nad = NAD as u128 / 10;
    // At target utilization, one year at 10% grows credit 300 -> 330,
    // margin 200 -> 220, and hLP funding 200 -> 220.
    let receipt = accrue_side::<true>(&mut market, MarketAsset::Base, slots_for_ms(MS_PER_YEAR), Some(50_000_000))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.credit_interest, 30);
    assert_eq!(receipt.margin_interest, 20);
    assert_eq!(receipt.hlp_interest, 20);
    assert_eq!(market.base_side.reserves.live_reserve, 1_050);
    assert_eq!(market.base_side.reserves.cash_reserve, 300);
    assert_eq!(receipt.from_slot, 0);
    assert_eq!(receipt.to_slot, slots_for_ms(MS_PER_YEAR));
    assert!(accrue_side::<true>(&mut market, MarketAsset::Base, receipt.to_slot, Some(50_000_000)).unwrap().is_none());
}

#[test]
fn short_checkpoints_preserve_fractional_borrow_interest() {
    let mut frequent = test_market(300, 1_000);
    configure_active_base_side(&mut frequent);
    frequent.debt.base_rate_at_target_nad = INTEREST_MIN_RATE_AT_TARGET_NAD;
    let mut single = frequent.clone();

    // Two hundred 200 ms slots accrue enough at the minimum APR to advance
    // the integer index while exercising its fractional remainder.
    for slot in 1..=200 {
        accrue_side::<false>(&mut frequent, MarketAsset::Base, slot, Some(50_000_000)).unwrap();
    }
    accrue_side::<false>(&mut single, MarketAsset::Base, 200, Some(50_000_000)).unwrap();

    assert!(frequent.debt.base_borrow_index_nad > NAD as u128);
    assert_eq!(frequent.debt.base_borrow_index_nad, single.debt.base_borrow_index_nad);
    assert!(frequent.debt.base_borrow_index_remainder > 0);
}

#[test]
fn accrual_receipts_preserve_each_buckets_rounding_boundary() {
    let mut market = test_market(3, 1_000);
    market.debt.fixed_base_shares = 3;
    market.debt.isolated_base_shares = 2;
    market.quote_hlp_vault.debt_shares = 2;
    market.quote_hlp_vault.base_hlp_live_reserve = 2;
    market.base_side.reserves.live_reserve = 10;
    market.debt.base_rate_at_target_nad = NAD as u128 / 2;
    let receipt = accrue_side::<true>(&mut market, MarketAsset::Base, slots_for_ms(MS_PER_YEAR), Some(50_000_000))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.credit_interest, 1);
    assert_eq!(receipt.margin_interest, 1);
    assert_eq!(receipt.hlp_interest, 1);
    assert_eq!(market.base_side.reserves.live_reserve, 12);
}

#[test]
fn no_time_elapsed_is_a_noop() {
    let mut market = test_market(1_000, 1_000);
    market.debt.base_last_accrual_slot = 100;
    market.debt.quote_last_accrual_slot = 100;
    market.accrue_interest_to_slot(100).unwrap();
    assert_eq!(market.debt.quote_borrow_index_nad, NAD as u128);
    assert_eq!(
        market.debt.quote_rate_at_target_nad,
        INTEREST_INITIAL_RATE_AT_TARGET_NAD
    );
    assert_eq!(market.debt.base_last_accrual_slot, 100);
    assert_eq!(market.debt.quote_last_accrual_slot, 100);
}

#[test]
fn debt_free_side_holds_the_current_rate() {
    let mut market = test_market(1_000_000, 1_000_000);
    market.accrue_interest_to_slot(slots_for_ms(MS_PER_YEAR)).unwrap();
    assert_eq!(market.debt.quote_rate_at_target_nad, INTEREST_INITIAL_RATE_AT_TARGET_NAD);
}

#[test]
fn missing_junior_doubles_rate_over_one_half_life() {
    let mut market = test_market(1_000_000, 150);
    market.quote_side.reserves.live_reserve = 1_000;
    market.base_hlp_vault.debt_shares = 850;
    market.base_hlp_vault.quote_hlp_live_reserve = 850;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    market.accrue_interest_to_slot(slots_for_ms(market.config.irm.half_life_ms)).unwrap();
    assert_eq!(market.debt.quote_rate_at_target_nad, 80_000_000);
    // Integral of 4% * 2^(t / 14 days): roughly 0.2213% over 14 days.
    assert!((1_002_210_000..1_002_220_000).contains(&market.debt.quote_borrow_index_nad));
}

#[test]
fn hlp_funding_interest_does_not_grow_virtual_reserve() {
    // hLP funding debt counts toward utilization and accrues interest, but
    // it is not same-side cash-backed reserve debt. Its interest reduces
    // hLP NAV without growing virtual reserves.
    let mut market = test_market(1_000_000, 150);
    market.quote_side.reserves.live_reserve = 1_000;
    market.base_hlp_vault.debt_shares = 850;
    market.base_hlp_vault.debt_principal = 850;
    market.base_hlp_vault.quote_hlp_live_reserve = 850;

    market.config.irm.initial_apr_nad = NAD / 10;
    market.config.irm.minimum_apr_nad = NAD / 10;
    market.config.irm.maximum_apr_nad = NAD / 10;
    market.debt.quote_rate_at_target_nad = (NAD / 10) as u128;
    market.accrue_interest_to_slot(slots_for_ms(MS_PER_YEAR)).unwrap();

    assert_eq!(market.debt.quote_borrow_index_nad, (NAD as u128) * 110 / 100);
    assert_eq!(market.quote_side.reserves.cash_reserve, 150);
    assert_eq!(market.quote_side.reserves.live_reserve, 1_000);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn cash_backed_interest_increases_virtual_reserve_with_debt() {
    // Normal cash-backed debt still grows virtual reserves as interest
    // accrues while the debt is unpaid.
    let mut market = test_market(1_000_000, 150);
    market.quote_side.reserves.live_reserve = 1_000;
    market.debt.fixed_quote_shares = 850;
    market.debt.fixed_quote_principal = 850;

    market.config.irm.initial_apr_nad = NAD / 10;
    market.config.irm.minimum_apr_nad = NAD / 10;
    market.config.irm.maximum_apr_nad = NAD / 10;
    market.debt.quote_rate_at_target_nad = (NAD / 10) as u128;
    market.accrue_interest_to_slot(slots_for_ms(MS_PER_YEAR)).unwrap();

    assert_eq!(market.debt.quote_borrow_index_nad, (NAD as u128) * 110 / 100);
    assert_eq!(market.quote_side.reserves.cash_reserve, 150);
    assert_eq!(market.quote_side.reserves.live_reserve, 1_085);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn margin_and_hlp_debt_both_count_toward_utilization() {
    // Quote debt = 480 margin + 480 base-hLP = 960 borrowed, 40 cash -> 96%
    // (> target), so the anchor must rise. If either leg were ignored, util
    // would fall below target and the anchor would instead drop.
    let mut market = test_market(1_000_000, 40);
    market.quote_side.reserves.live_reserve = 1_000;
    market.debt.fixed_quote_shares = 480;
    market.base_hlp_vault.debt_shares = 480;
    market.base_hlp_vault.quote_hlp_live_reserve = 480;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    market.accrue_interest_to_slot(slots_for_ms(MS_PER_YEAR)).unwrap();
    assert!(market.debt.quote_rate_at_target_nad > INTEREST_INITIAL_RATE_AT_TARGET_NAD);
}

#[test]
fn explicitly_configured_ceiling_caps_sustained_pressure() {
    // ~100% utilization held for years: the anchor ramps up (capped per
    // step) and clamps at the max, never exceeding it.
    let mut market = test_market(1_000_000, 1);
    market.config.irm.maximum_apr_nad = INTEREST_MAX_RATE_AT_TARGET_NAD as u64;
    market.quote_side.reserves.live_reserve = 10_001;
    market.base_hlp_vault.debt_shares = 10_000;
    market.base_hlp_vault.quote_hlp_live_reserve = 10_000;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    for year in 1..=15u64 {
        market
            .accrue_interest_to_slot(slots_for_ms(MS_PER_YEAR * year))
            .unwrap();
    }
    assert_eq!(market.debt.quote_rate_at_target_nad, INTEREST_MAX_RATE_AT_TARGET_NAD);
}

#[test]
fn mixed_active_and_idle_sides_accrue_independently_with_shared_coverage_snapshot() {
    let current_slot = slots_for_ms(MS_PER_YEAR);
    let mut market = test_market(300, 1_000);
    configure_active_base_side(&mut market);
    market.config.irm.minimum_apr_nad = INTEREST_INITIAL_RATE_AT_TARGET_NAD as u64;
    market.config.irm.maximum_apr_nad = INTEREST_INITIAL_RATE_AT_TARGET_NAD as u64;
    market.debt.quote_last_accrual_slot = current_slot / 2;
    let expected_quote_rate = INTEREST_INITIAL_RATE_AT_TARGET_NAD;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();

    Debt::reset_shares_to_debt_call_count();
    market.accrue_interest_to_slot(current_slot).unwrap();

    // Coverage snapshots add three exposure conversions. The idle side
    // performs no debt conversion.
    assert_eq!(Debt::shares_to_debt_call_count(), 6);
    assert_eq!(market.debt.base_borrow_index_nad, (NAD as u128) * 104 / 100);
    assert_eq!(market.debt.quote_borrow_index_nad, NAD as u128);
    assert_eq!(market.debt.base_rate_at_target_nad, INTEREST_INITIAL_RATE_AT_TARGET_NAD);
    assert_eq!(market.debt.quote_rate_at_target_nad, expected_quote_rate);
    assert_eq!(market.base_side.reserves.live_reserve, 1_020);
    assert_eq!(market.quote_side.reserves.live_reserve, 1_000);
    assert_eq!(market.debt.base_last_accrual_slot, current_slot);
    assert_eq!(market.debt.quote_last_accrual_slot, current_slot);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn debt_free_sides_skip_all_debt_and_index_work() {
    let current_slot = slots_for_ms(MS_PER_YEAR);
    let mut market = test_market(1_000, 2_000);

    // These sentinel indexes would overflow if the active-side index path
    // ran. A debt-free side must hold its rate and advance its own
    // accrual slot.
    market.debt.base_borrow_index_nad = u128::MAX;
    market.debt.quote_borrow_index_nad = u128::MAX;
    let expected_idle_rate = INTEREST_INITIAL_RATE_AT_TARGET_NAD;

    Debt::reset_shares_to_debt_call_count();
    market.accrue_interest_to_slot(current_slot).unwrap();

    assert_eq!(Debt::shares_to_debt_call_count(), 0);
    assert_eq!(market.debt.base_borrow_index_nad, u128::MAX);
    assert_eq!(market.debt.quote_borrow_index_nad, u128::MAX);
    assert_eq!(market.base_side.reserves.live_reserve, 1_000);
    assert_eq!(market.quote_side.reserves.live_reserve, 2_000);
    assert_eq!(market.debt.base_rate_at_target_nad, expected_idle_rate);
    assert_eq!(market.debt.quote_rate_at_target_nad, expected_idle_rate);
    assert_eq!(market.debt.base_last_accrual_slot, current_slot);
    assert_eq!(market.debt.quote_last_accrual_slot, current_slot);
}

#[test]
fn long_slot_gap_charges_the_full_elapsed_interval() {
    let mut market = test_market(300, 1_000);
    configure_active_base_side(&mut market);
    market.config.irm.minimum_apr_nad = INTEREST_INITIAL_RATE_AT_TARGET_NAD as u64;
    market.config.irm.maximum_apr_nad = INTEREST_INITIAL_RATE_AT_TARGET_NAD as u64;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();

    Debt::reset_shares_to_debt_call_count();
    let current_slot = slots_for_ms(2 * MS_PER_YEAR);
    market.accrue_interest_to_slot(current_slot).unwrap();

    // At target utilization, two years at the 4% initial rate grow the index
    // by 8% even when no other operation checkpoints the market in between.
    assert_eq!(Debt::shares_to_debt_call_count(), 6);
    assert_eq!(market.debt.base_borrow_index_nad, (NAD as u128) * 108 / 100);
    assert_eq!(market.base_side.reserves.live_reserve, 1_040);
    assert_eq!(market.debt.base_last_accrual_slot, current_slot);
    assert_eq!(market.debt.quote_last_accrual_slot, current_slot);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

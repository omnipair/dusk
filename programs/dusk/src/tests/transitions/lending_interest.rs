use super::*;
use crate::state::DEFAULT_IRM_ADJUSTMENT_SPEED_PER_YEAR;

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
        version: MARKET_LAYOUT_VERSION,
        ylp_mint: Pubkey::new_unique(),
        base_side,
        quote_side,
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
fn idle_side_drifts_anchor_down_toward_min() {
    // Cash present, zero debt -> utilization 0 -> error -1 -> anchor falls.
    let mut market = test_market(1_000_000, 1_000_000);
    market.accrue_interest_to_slot(slots_for_ms(MS_PER_YEAR)).unwrap();
    assert!(market.debt.quote_rate_at_target_nad < INTEREST_INITIAL_RATE_AT_TARGET_NAD);
    assert!(market.debt.quote_rate_at_target_nad >= INTEREST_MIN_RATE_AT_TARGET_NAD);
}

#[test]
fn high_utilization_raises_anchor_and_accrues_index() {
    // Quote borrowed 850 via base-hLP, 150 cash -> util 85% (above 70% target).
    // error = +0.5 -> curve mult 2.5x -> rate = 4% * 2.5 = 10% APR.
    let mut market = test_market(1_000_000, 150);
    market.quote_side.reserves.live_reserve = 1_000;
    market.base_hlp_vault.debt_shares = 850;
    market.base_hlp_vault.quote_hlp_live_reserve = 850;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    market.accrue_interest_to_slot(slots_for_ms(MS_PER_YEAR)).unwrap();
    // 10% APR over one year compounds the index to 1.10.
    assert_eq!(market.debt.quote_borrow_index_nad, (NAD as u128) * 110 / 100);
    // Anchor drifted up (util above target).
    assert!(market.debt.quote_rate_at_target_nad > INTEREST_INITIAL_RATE_AT_TARGET_NAD);
    assert!(market.debt.quote_rate_at_target_nad <= INTEREST_MAX_RATE_AT_TARGET_NAD);
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
fn anchor_saturates_at_max_under_sustained_pressure() {
    // ~100% utilization held for years: the anchor ramps up (capped per
    // step) and clamps at the max, never exceeding it.
    let mut market = test_market(1_000_000, 1);
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
fn mixed_active_and_idle_sides_accrue_independently_with_three_conversions() {
    let current_slot = slots_for_ms(MS_PER_YEAR);
    let mut market = test_market(300, 1_000);
    configure_active_base_side(&mut market);
    market.debt.quote_last_accrual_slot = current_slot / 2;
    let quote_elapsed_ms = (current_slot - market.debt.quote_last_accrual_slot).saturating_mul(TARGET_MS_PER_SLOT);
    let expected_quote_rate = adapt_rate_at_target_nad(
        INTEREST_INITIAL_RATE_AT_TARGET_NAD,
        -(NAD as i128),
        quote_elapsed_ms,
        DEFAULT_IRM_ADJUSTMENT_SPEED_PER_YEAR as u128,
        INTEREST_MIN_RATE_AT_TARGET_NAD,
        INTEREST_MAX_RATE_AT_TARGET_NAD,
        INTEREST_MAX_ADAPTATION_STEP_NAD,
    )
    .unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();

    Debt::reset_shares_to_debt_call_count();
    market.accrue_interest_to_slot(current_slot).unwrap();

    // The active base side performs one hLP conversion before accrual and
    // one conversion for each cash-backed bucket afterward. The idle quote
    // side performs no debt conversion at all.
    assert_eq!(Debt::shares_to_debt_call_count(), 3);
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
    // ran. A debt-free side must only decay its anchor and advance its own
    // accrual slot.
    market.debt.base_borrow_index_nad = u128::MAX;
    market.debt.quote_borrow_index_nad = u128::MAX;
    let expected_idle_rate = adapt_rate_at_target_nad(
        INTEREST_INITIAL_RATE_AT_TARGET_NAD,
        -(NAD as i128),
        current_slot.saturating_mul(TARGET_MS_PER_SLOT),
        DEFAULT_IRM_ADJUSTMENT_SPEED_PER_YEAR as u128,
        INTEREST_MIN_RATE_AT_TARGET_NAD,
        INTEREST_MAX_RATE_AT_TARGET_NAD,
        INTEREST_MAX_ADAPTATION_STEP_NAD,
    )
    .unwrap();

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
fn long_slot_gap_saturates_elapsed_time_without_extra_conversions() {
    let mut market = test_market(300, 1_000);
    configure_active_base_side(&mut market);
    market.debt.base_last_accrual_slot = 7;
    market.debt.quote_last_accrual_slot = 11;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();

    Debt::reset_shares_to_debt_call_count();
    market.accrue_interest_to_slot(u64::MAX).unwrap();

    // Slot-to-millisecond conversion saturates, then the interest model's
    // established one-year cap applies. At target utilization, this is one
    // year at the 4% initial rate.
    assert_eq!(Debt::shares_to_debt_call_count(), 3);
    assert_eq!(market.debt.base_borrow_index_nad, (NAD as u128) * 104 / 100);
    assert_eq!(market.base_side.reserves.live_reserve, 1_020);
    assert_eq!(market.debt.base_last_accrual_slot, u64::MAX);
    assert_eq!(market.debt.quote_last_accrual_slot, u64::MAX);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

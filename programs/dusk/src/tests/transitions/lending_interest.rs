use super::*;

// Market-level interest checkpoints and debt-accounting invariants.
use crate::{
    constants::{DEFAULT_BORROW_APR_NAD, MARKET_LAYOUT_VERSION, MS_PER_YEAR, NAD, TARGET_MS_PER_SLOT},
    state::{Debt, HlpVault, MarketConfig, MarketSide, Reserves, Risk},
};

fn slots_for_ms(ms: u64) -> u64 {
    ms / TARGET_MS_PER_SLOT
}

fn rate_input(signal_nad: Option<u128>) -> crate::math::borrow_controller::RiskRateInput {
    crate::math::borrow_controller::RiskRateInput {
        signal_nad,
        utilization_bps: 7_000,
    }
}

fn test_market(base_cash: u64, quote_cash: u64) -> Market {
    let base_side = MarketSide {
        reserves: Reserves {
            live_reserve: u128::from(base_cash),
            cash_reserve: base_cash,
            ..Reserves::default()
        },
        ..MarketSide::default()
    };
    let quote_side = MarketSide {
        reserves: Reserves {
            live_reserve: u128::from(quote_cash),
            cash_reserve: quote_cash,
            ..Reserves::default()
        },
        ..MarketSide::default()
    };
    Market {
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
        config: MarketConfig::default(),
        amm: Default::default(),
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            base_borrow_apr_nad: DEFAULT_BORROW_APR_NAD,
            quote_borrow_apr_nad: DEFAULT_BORROW_APR_NAD,
            base_last_accrual_slot: 0,
            quote_last_accrual_slot: 0,
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
    }
}

fn configure_active_base_side(market: &mut Market) {
    // At index 1.0: 300 fixed + 200 isolated + 200 hLP funding debt,
    // backed by 300 idle cash. The quote side remains debt-free.
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
    market.debt.base_borrow_apr_nad = NAD as u128 / 10;
    // One year at a held 10% APR grows credit 300 -> 330,
    // margin 200 -> 220, and hLP funding 200 -> 220.
    let receipt = accrue_side::<true>(
        &mut market,
        MarketAsset::Base,
        slots_for_ms(MS_PER_YEAR),
        rate_input(Some(50_000_000)),
    )
    .unwrap()
    .unwrap();
    assert_eq!(receipt.credit_interest, 30);
    assert_eq!(receipt.margin_interest, 20);
    assert_eq!(receipt.hlp_interest, 20);
    assert_eq!(market.base_side.reserves.live_reserve, 1_050);
    assert_eq!(market.base_side.reserves.cash_reserve, 300);
    assert_eq!(receipt.from_slot, 0);
    assert_eq!(receipt.to_slot, slots_for_ms(MS_PER_YEAR));
    assert!(
        accrue_side::<true>(&mut market, MarketAsset::Base, receipt.to_slot, rate_input(Some(50_000_000)))
            .unwrap()
            .is_none()
    );
}

#[test]
fn short_checkpoints_preserve_fractional_borrow_interest() {
    let mut frequent = test_market(300, 1_000);
    configure_active_base_side(&mut frequent);
    frequent.debt.base_borrow_apr_nad = crate::transitions::tranches::MINIMUM_BORROW_APR_NAD as u128;
    let mut single = frequent.clone();

    // Two hundred 200 ms slots accrue enough at the minimum APR to advance
    // the integer index while exercising its fractional remainder.
    for slot in 1..=200 {
        accrue_side::<false>(&mut frequent, MarketAsset::Base, slot, rate_input(Some(50_000_000))).unwrap();
    }
    accrue_side::<false>(&mut single, MarketAsset::Base, 200, rate_input(Some(50_000_000))).unwrap();

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
    market.debt.base_borrow_apr_nad = NAD as u128 / 2;
    let receipt = accrue_side::<true>(
        &mut market,
        MarketAsset::Base,
        slots_for_ms(MS_PER_YEAR),
        rate_input(Some(50_000_000)),
    )
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
        market.debt.quote_borrow_apr_nad,
        DEFAULT_BORROW_APR_NAD
    );
    assert_eq!(market.debt.base_last_accrual_slot, 100);
    assert_eq!(market.debt.quote_last_accrual_slot, 100);
}

#[test]
fn debt_free_side_holds_the_current_rate() {
    let mut market = test_market(1_000_000, 1_000_000);
    market.accrue_interest_to_slot(slots_for_ms(MS_PER_YEAR)).unwrap();
    assert_eq!(
        market.debt.quote_borrow_apr_nad,
        DEFAULT_BORROW_APR_NAD
    );
}

#[test]
fn missing_junior_doubles_rate_over_one_half_life() {
    let mut market = test_market(1_000_000, 150);
    market.quote_side.reserves.live_reserve = 1_000;
    market.base_hlp_vault.debt_shares = 850;
    market.base_hlp_vault.quote_hlp_live_reserve = 850;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    market
        .accrue_interest_to_slot(slots_for_ms(market.config.irm.half_life_ms))
        .unwrap();
    assert_eq!(market.debt.quote_borrow_apr_nad, 80_000_000);
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
    market.debt.quote_borrow_apr_nad = (NAD / 10) as u128;
    // Isolate ledger ownership at a held coverage-band rate; adaptive
    // direction and the fixed floor are covered by controller tests.
    accrue_side::<false>(
        &mut market,
        MarketAsset::Quote,
        slots_for_ms(MS_PER_YEAR),
        rate_input(Some(50_000_000)),
    )
    .unwrap();

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
    market.debt.quote_borrow_apr_nad = (NAD / 10) as u128;
    // Isolate ledger ownership at a held coverage-band rate; adaptive
    // direction and the fixed floor are covered by controller tests.
    accrue_side::<false>(
        &mut market,
        MarketAsset::Quote,
        slots_for_ms(MS_PER_YEAR),
        rate_input(Some(50_000_000)),
    )
    .unwrap();

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
    assert!(market.debt.quote_borrow_apr_nad > DEFAULT_BORROW_APR_NAD);
}

#[test]
fn sustained_pressure_has_no_governed_economic_ceiling() {
    let mut market = test_market(1_000_000, 1);
    market.quote_side.reserves.live_reserve = 10_001;
    market.base_hlp_vault.debt_shares = 10_000;
    market.base_hlp_vault.quote_hlp_live_reserve = 10_000;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    market
        .accrue_interest_to_slot(slots_for_ms(10 * market.config.irm.half_life_ms))
        .unwrap();
    assert!(market.debt.quote_borrow_apr_nad > 2 * NAD as u128);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn mixed_active_and_idle_sides_accrue_independently_with_shared_coverage_snapshot() {
    let current_slot = slots_for_ms(MS_PER_YEAR);
    let mut market = test_market(300, 1_000);
    configure_active_base_side(&mut market);
    market.debt.quote_last_accrual_slot = current_slot / 2;
    let expected_quote_rate = DEFAULT_BORROW_APR_NAD;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();

    let integral = crate::math::borrow_controller::accrue_risk_rate(
        &market.config.irm,
        DEFAULT_BORROW_APR_NAD * NAD as u128,
        crate::math::borrow_controller::RiskRateInput {
            signal_nad: Some(0),
            utilization_bps: 7_000,
        },
        MS_PER_YEAR,
    )
    .unwrap();
    let expected_index = NAD as u128 + integral.integrated_apr_nad_ms / MS_PER_YEAR as u128;
    market.accrue_interest_to_slot(current_slot).unwrap();

    assert_eq!(market.debt.base_borrow_index_nad, expected_index);
    assert_eq!(market.debt.quote_borrow_index_nad, NAD as u128);
    assert_eq!(market.debt.base_borrow_apr_nad, integral.end_apr_e18 / NAD as u128);
    assert_eq!(market.debt.quote_borrow_apr_nad, expected_quote_rate);
    assert_eq!(
        market.base_side.reserves.live_reserve as u128,
        500 + 300 * expected_index / NAD as u128 + 200 * expected_index / NAD as u128
    );
    assert_eq!(market.quote_side.reserves.live_reserve, 1_000);
    assert_eq!(market.debt.base_last_accrual_slot, current_slot);
    assert_eq!(market.debt.quote_last_accrual_slot, current_slot);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn debt_free_sides_reset_the_index_without_debt_arithmetic() {
    let current_slot = slots_for_ms(MS_PER_YEAR);
    let mut market = test_market(1_000, 2_000);

    // These sentinel indexes would overflow if the active-side index path
    // ran. A debt-free side must hold its rate and advance its own
    // accrual slot.
    market.debt.base_borrow_index_nad = u128::MAX;
    market.debt.quote_borrow_index_nad = u128::MAX;
    let expected_idle_rate = DEFAULT_BORROW_APR_NAD;

    market.accrue_interest_to_slot(current_slot).unwrap();

    assert_eq!(market.debt.base_borrow_index_nad, NAD as u128);
    assert_eq!(market.debt.quote_borrow_index_nad, NAD as u128);
    assert_eq!(market.base_side.reserves.live_reserve, 1_000);
    assert_eq!(market.quote_side.reserves.live_reserve, 2_000);
    assert_eq!(market.debt.base_borrow_apr_nad, expected_idle_rate);
    assert_eq!(market.debt.quote_borrow_apr_nad, expected_idle_rate);
    assert_eq!(market.debt.base_last_accrual_slot, current_slot);
    assert_eq!(market.debt.quote_last_accrual_slot, current_slot);
}

#[test]
fn long_slot_gap_charges_the_full_elapsed_interval() {
    let mut market = test_market(300, 1_000);
    configure_active_base_side(&mut market);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();

    let integral = crate::math::borrow_controller::accrue_risk_rate(
        &market.config.irm,
        DEFAULT_BORROW_APR_NAD * NAD as u128,
        crate::math::borrow_controller::RiskRateInput {
            signal_nad: Some(0),
            utilization_bps: 7_000,
        },
        2 * MS_PER_YEAR,
    )
    .unwrap();
    let expected_index = NAD as u128 + integral.integrated_apr_nad_ms / MS_PER_YEAR as u128;
    let current_slot = slots_for_ms(2 * MS_PER_YEAR);
    market.accrue_interest_to_slot(current_slot).unwrap();

    // Two years of zero junior coverage charge the full adaptive integral,
    // even without intermediate checkpoints or an economic APR ceiling.
    assert_eq!(market.debt.base_borrow_index_nad, expected_index);
    assert_eq!(
        market.base_side.reserves.live_reserve as u128,
        500 + 300 * expected_index / NAD as u128 + 200 * expected_index / NAD as u128
    );
    assert_eq!(market.debt.base_last_accrual_slot, current_slot);
    assert_eq!(market.debt.quote_last_accrual_slot, current_slot);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

#[test]
fn protocol_markup_is_added_to_the_base_borrow_rate() {
    let mut market = test_market(300, 1_000);
    configure_active_base_side(&mut market);
    market.debt.base_borrow_apr_nad = NAD as u128 / 10;
    market.protocol_interest_markup_bps = 2_000;
    let receipt = accrue_side::<true>(
        &mut market,
        MarketAsset::Base,
        slots_for_ms(MS_PER_YEAR),
        rate_input(Some(50_000_000)),
    )
    .unwrap()
    .unwrap();
    assert_eq!(receipt.credit_interest, 36);
    assert_eq!(receipt.margin_interest, 24);
    assert_eq!(receipt.hlp_interest, 24);
    assert_eq!(market.debt.base_borrow_apr_nad, NAD as u128 / 10);
}

#[test]
fn long_idle_uncapped_interest_saturates_without_corrupting_shared_debt() {
    let scale = 1_000_000_000_u64;
    let mut market = test_market(300 * scale, 1_000 * scale);
    configure_active_base_side(&mut market);
    market.base_side.reserves.live_reserve = u128::from(1_000 * scale);
    market.base_side.reserves.cash_reserve = 300 * scale;
    market.debt.fixed_base_shares = 300 * scale as u128;
    market.debt.fixed_base_principal = 300 * scale;
    market.debt.isolated_base_shares = 200 * scale as u128;
    market.debt.isolated_base_principal = 200 * scale;
    market.quote_hlp_vault.debt_shares = 200 * scale as u128;
    market.quote_hlp_vault.debt_principal = 200 * scale;
    market.quote_hlp_vault.base_hlp_live_reserve = 200 * scale;
    market.config.irm.half_life_ms = crate::constants::MS_PER_DAY / 2;
    market.amm.launch_reference_price_nad = NAD;
    market.amm.price_ema_nad = NAD;
    market.risk.base_price_ema_nad = NAD;
    let curve = market.curve_reserve(MarketAsset::Base).unwrap();
    let slot = slots_for_ms(60 * crate::constants::MS_PER_DAY);
    market.accrue_interest_to_slot(slot).unwrap();
    assert_eq!(market.debt.base_last_accrual_slot, slot);
    assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), curve);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    assert!(market.base_side.reserves.live_reserve > u64::MAX as u128);
    assert!(market.debt.fixed_base_debt().unwrap() <= u64::MAX as u128);
    assert!(market.public_unpaid_interest(MarketAsset::Base).unwrap() <= u64::MAX as u128);
    let saturated = market.debt.base_borrow_index_nad;
    market.accrue_interest_to_slot(slot + 1).unwrap();
    assert_eq!(market.debt.base_borrow_index_nad, saturated);
    assert_eq!(market.debt.base_borrow_index_remainder, 0);
    assert_eq!(market.debt.base_last_accrual_slot, slot + 1);
}

fn numeric_limit_market() -> Market {
    let mut market = test_market(1_000_000_000_000, 1_000_000_000_000);
    market.debt.fixed_base_shares = 1_000_000_000_000;
    market.debt.fixed_base_principal = 1_000_000_000_000;
    market.base_side.reserves.live_reserve += 1_000_000_000_000;
    market.config.irm.half_life_ms = crate::constants::MS_PER_DAY / 2;
    market.base_side.shares.srlp_supply = 1_000_000_000_000;
    market.quote_side.shares.srlp_supply = 1_000_000_000_000;
    market.amm.launch_reference_price_nad = NAD;
    market.amm.price_ema_nad = NAD;
    market.risk.base_price_ema_nad = NAD;
    market.prepare_amm_for_swap(0).unwrap();
    market.refresh_risk().unwrap();
    market
}

#[test]
fn saturation_books_matching_lp_and_fee_receivables_without_inventing_cash() {
    let mut market = numeric_limit_market();
    market.base_side.fees.junior.initialized = true;
    market.quote_side.fees.junior.initialized = true;
    market.protocol_interest_markup_bps = 10_000;
    market.operator.interest_share_bps = 5_000;
    let slot = slots_for_ms(60 * crate::constants::MS_PER_DAY);
    accrue_side::<false>(&mut market, MarketAsset::Base, slot, rate_input(Some(0))).unwrap();
    let books = market.income.get().unwrap();
    let revenue = *books.revenue[0];
    let unpaid_lp = books.books[0].unpaid;
    assert_eq!(market.public_unpaid_interest(MarketAsset::Base).unwrap(), revenue.gross_unpaid as u128);
    assert_eq!(revenue.gross_unpaid, unpaid_lp + revenue.unpaid_fees().unwrap());
    assert!(revenue.operator_unpaid > 0 && revenue.protocol_unpaid > 0 && unpaid_lp > 0);
    assert_eq!(revenue.operator_claimable, 0);
    assert_eq!(market.base_side.fees.interest_vault_balance, 0);
    let index = market.debt.base_borrow_index_nad;
    accrue_side::<false>(&mut market, MarketAsset::Base, slot + 100, rate_input(Some(0))).unwrap();
    assert_eq!(market.debt.base_borrow_index_nad, index);
    assert_eq!(*market.income.get().unwrap().revenue[0], revenue);
}

#[test]
fn fully_repaid_side_resets_the_index_without_changing_its_live_apr() {
    let mut market = numeric_limit_market();
    market.debt.fixed_base_shares = 0;
    market.debt.fixed_base_principal = 0;
    market.base_side.reserves.live_reserve = market.base_side.reserves.cash_reserve as u128;
    market.debt.base_borrow_index_nad = u64::MAX as u128 * NAD as u128;
    market.debt.base_borrow_index_remainder = 1;
    let apr = market.debt.base_borrow_apr_nad;
    accrue_side::<false>(&mut market, MarketAsset::Base, 1, rate_input(Some(0))).unwrap();
    assert_eq!(market.debt.base_borrow_index_nad, NAD as u128);
    assert_eq!(market.debt.base_borrow_apr_nad, apr);
    assert_eq!(Debt::debt_to_shares(1, market.debt.base_borrow_index_nad).unwrap(), 1);
}

#[test]
fn final_numeric_limit_handles_a_demand_beyond_the_u128_index_range() {
    let mut market = numeric_limit_market();
    market.debt.fixed_base_shares = 1;
    market.debt.fixed_base_principal = 1;
    market.debt.base_borrow_index_nad = (u64::MAX as u128 / 2) * NAD as u128;
    market.debt.base_borrow_apr_nad = u64::MAX as u128;
    market.base_side.reserves.live_reserve = market.base_side.reserves.cash_reserve as u128
        + market.debt.fixed_base_debt().unwrap();
    let slot = slots_for_ms(1_000 * MS_PER_YEAR);
    accrue_side::<false>(&mut market, MarketAsset::Base, slot, rate_input(Some(0))).unwrap();
    assert_eq!(market.debt.fixed_base_debt().unwrap(), u64::MAX as u128);
    assert_eq!(market.debt.base_last_accrual_slot, slot);
    assert_eq!(market.debt.base_borrow_index_remainder, 0);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

#[test]
fn saturation_allows_partial_repayment_then_resumes_only_future_interest() {
    let mut market = numeric_limit_market();
    let mut position = BorrowPosition {
        fixed_base_shares: market.debt.fixed_base_shares,
        fixed_base_principal: market.debt.fixed_base_principal,
        auction_debt_asset: u8::MAX,
        ..BorrowPosition::default()
    };
    let slot = slots_for_ms(60 * crate::constants::MS_PER_DAY);
    accrue_side::<false>(&mut market, MarketAsset::Base, slot, rate_input(Some(0))).unwrap();
    let prior_debt = market.debt.fixed_base_debt().unwrap();
    let repay = market.fixed_repayment_for_max(&position, MarketAsset::Base, u64::MAX / 3).unwrap();
    market.repay(&mut position, MarketAsset::Base, repay.cash_repaid).unwrap();
    let reduced_debt = market.debt.fixed_base_debt().unwrap();
    assert!(reduced_debt < prior_debt);
    assert!(accrue_side::<true>(&mut market, MarketAsset::Base, slot, rate_input(Some(0))).unwrap().is_none());
    assert_eq!(market.debt.fixed_base_debt().unwrap(), reduced_debt);
    let receipt = accrue_side::<true>(&mut market, MarketAsset::Base, slot + 1, rate_input(Some(0))).unwrap().unwrap();
    assert_eq!(receipt.from_slot, slot);
    assert!(receipt.credit_interest > 0);
    assert!(market.debt.fixed_base_debt().unwrap() <= u64::MAX as u128);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

#[test]
fn saturation_allows_full_repayment_across_the_signed_u64_boundary() {
    let mut market = numeric_limit_market();
    let mut position = BorrowPosition {
        fixed_base_shares: market.debt.fixed_base_shares,
        fixed_base_principal: market.debt.fixed_base_principal,
        auction_debt_asset: u8::MAX,
        ..BorrowPosition::default()
    };
    let slot = slots_for_ms(60 * crate::constants::MS_PER_DAY);
    accrue_side::<false>(&mut market, MarketAsset::Base, slot, rate_input(Some(0))).unwrap();
    let debt = u64::try_from(position.fixed_base_debt(&market.debt).unwrap()).unwrap();
    assert!(debt > i64::MAX as u64);
    let receipt = market.repay(&mut position, MarketAsset::Base, debt).unwrap();
    assert_eq!(receipt.debt_delta, -i128::from(debt));
    assert_eq!(position.fixed_base_shares, 0);
    assert_eq!(market.debt.fixed_base_principal, 0);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    accrue_side::<false>(&mut market, MarketAsset::Base, slot + 1, rate_input(None)).unwrap();
    assert_eq!(market.debt.base_borrow_index_nad, NAD as u128);
}

#[test]
fn exhausted_income_unit_capacity_pauses_accrual_and_keeps_repayments_available() {
    let mut market = numeric_limit_market();
    market.base_side.fees.junior.initialized = true;
    market.quote_side.fees.junior.initialized = true;
    let mut position = BorrowPosition {
        fixed_base_shares: market.debt.fixed_base_shares,
        fixed_base_principal: market.debt.fixed_base_principal,
        auction_debt_asset: u8::MAX,
        ..BorrowPosition::default()
    };
    let slot = slots_for_ms(60 * crate::constants::MS_PER_DAY);
    accrue_side::<false>(&mut market, MarketAsset::Base, slot, rate_input(Some(0))).unwrap();
    let pay = market.fixed_repayment_for_max(&position, MarketAsset::Base, u64::MAX / 2).unwrap();
    let receipt = market.repay(&mut position, MarketAsset::Base, pay.cash_repaid).unwrap();
    market.record_receivable_income_credit(MarketAsset::Base, false, receipt.interest_paid, 0,
        crate::state::ProtocolAuctionSplit::default(), 0).unwrap();
    let remaining_before = market.debt.fixed_base_debt().unwrap();
    let end = slot + slots_for_ms(crate::constants::MS_PER_DAY);
    accrue_side::<false>(&mut market, MarketAsset::Base, end, rate_input(Some(0))).unwrap();
    let books = market.income.get().unwrap();
    assert!(books.books[0].units > u64::MAX - 2_000_000_000_000);
    assert_eq!(books.books[0].unpaid, books.revenue[0].gross_unpaid);
    assert!(market.debt.fixed_base_debt().unwrap() >= remaining_before);
    assert!(market.debt.fixed_base_debt().unwrap() < u64::MAX as u128);
    assert_eq!(market.debt.base_last_accrual_slot, end);
    assert_eq!(market.debt.base_borrow_index_remainder, 0);
    let pay = market.fixed_repayment_for_max(&position, MarketAsset::Base, u64::MAX / 4).unwrap();
    market.repay(&mut position, MarketAsset::Base, pay.cash_repaid).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(96))]
    #[test]
    fn numerical_boundary_conserves_all_three_debt_buckets_and_frozen_fees(
        principal in 1_u64..u64::MAX / 8,
        cash in 1_u64..u64::MAX / 8,
        markup in 0_u16..=10_000,
        operator_share in 0_u16..=5_000,
        days in 60_u64..3_650,
    ) {
        let mut market = numeric_limit_market();
        market.base_side.reserves.cash_reserve = cash;
        market.debt.fixed_base_shares = principal as u128;
        market.debt.fixed_base_principal = principal;
        market.debt.isolated_base_shares = principal as u128;
        market.debt.isolated_base_principal = principal;
        market.quote_hlp_vault.debt_shares = principal as u128;
        market.quote_hlp_vault.debt_principal = principal;
        market.quote_hlp_vault.base_hlp_live_reserve = principal;
        market.base_side.reserves.live_reserve = cash as u128 + 3 * principal as u128;
        market.base_side.fees.junior.initialized = true;
        market.quote_side.fees.junior.initialized = true;
        market.protocol_interest_markup_bps = markup;
        market.operator.interest_share_bps = operator_share;
        let curve = market.curve_reserve(MarketAsset::Base).unwrap();
        let slot = slots_for_ms(days * crate::constants::MS_PER_DAY);
        accrue_side::<false>(&mut market, MarketAsset::Base, slot, rate_input(Some(0))).unwrap();
        let public = market.public_unpaid_interest(MarketAsset::Base).unwrap();
        let funding = market.hlp_funding_debt(MarketAsset::Base).unwrap() - principal as u128;
        let income = market.income.get().unwrap();
        proptest::prop_assert_eq!(income.books[0].unpaid as u128 + income.revenue[0].unpaid_fees().unwrap() as u128, public);
        proptest::prop_assert_eq!(income.books[2].unpaid as u128 + income.revenue[2].unpaid_fees().unwrap() as u128, funding);
        proptest::prop_assert!(public <= u64::MAX as u128 && funding <= u64::MAX as u128);
        proptest::prop_assert!(market.debt.fixed_base_debt().unwrap() <= u64::MAX as u128);
        proptest::prop_assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), curve);
        proptest::prop_assert_eq!(market.debt.base_last_accrual_slot, slot);
        proptest::prop_assert_eq!(market.base_side.fees.interest_vault_balance, 0);
        market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    }
}

#[test]
fn saturated_interest_does_not_block_an_otherwise_cash_backed_lp_exit() {
    let mut market = numeric_limit_market();
    market.base_side.fees.junior.initialized = true;
    market.quote_side.fees.junior.initialized = true;
    let slot = slots_for_ms(60 * crate::constants::MS_PER_DAY);
    accrue_side::<false>(&mut market, MarketAsset::Base, slot, rate_input(Some(0))).unwrap();
    let before = (*market.income.get().unwrap().books[0]).clone();
    let receipt = market.remove_liquidity(100_000_000_000).unwrap();
    assert_eq!(receipt.base_amount_out, 200_000_000_000);
    assert_eq!(receipt.quote_amount_out, 100_000_000_000);
    assert_eq!(*market.income.get().unwrap().books[0], before);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

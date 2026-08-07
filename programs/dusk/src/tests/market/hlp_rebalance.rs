use super::*;
use crate::state::AmmConfig;
use crate::{
    constants::{BPS_DENOMINATOR, MARKET_LAYOUT_VERSION},
    math::{calculate_raw_amount_out, hlp_opposite_exposure_nad, ideal_hlp_rebalance_nad, market_spot_price_nad},
    state::{Insurance, MarketConfig, MarketSide, Risk},
};
use proptest::prelude::*;

fn checkpoint_hlp_vaults(market: &mut Market) -> Result<(i128, i128)> {
    market.checkpoint_hlp_vaults()
}

fn rebalance_hlp_vaults(market: &mut Market) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    let current_slot = curve_slot(market);
    let base = if market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0 {
        rebalance_one_hlp(market, MarketAsset::Base, current_slot)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Base)
    };
    let quote = if market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0 {
        rebalance_one_hlp(market, MarketAsset::Quote, current_slot)?
    } else {
        empty_hlp_rebalance_receipt(MarketAsset::Quote)
    };
    Ok((base, quote))
}

fn pre_solve_hlp_vaults_for_swap(
    market: &mut Market,
    asset_in: MarketAsset,
    amount_in: u64,
) -> Result<(HlpRebalanceReceipt, HlpRebalanceReceipt)> {
    let current_slot = curve_slot(market);
    let base = pre_solve_one_hlp_for_swap(
        market,
        MarketAsset::Base,
        asset_in,
        amount_in,
        amount_in,
        current_slot,
        asset_in,
        0,
    )?;
    let quote = pre_solve_one_hlp_for_swap(
        market,
        MarketAsset::Quote,
        asset_in,
        amount_in,
        amount_in,
        current_slot,
        asset_in,
        0,
    )?;
    Ok((base, quote))
}

fn require_hlp_swap_path_safe(
    market: &Market,
    start_price_nad: u64,
    end_price_nad: u64,
    base_residual_on_entry: bool,
    quote_residual_on_entry: bool,
) -> Result<()> {
    let start_prices = hlp_curve_prices_from_base_price_nad(start_price_nad as u128)?;
    let end_prices = hlp_curve_prices_from_base_price_nad(end_price_nad as u128)?;
    require_residual_hlp_swap_safe(
        market,
        MarketAsset::Base,
        start_prices,
        end_prices,
        base_residual_on_entry,
    )?;
    require_residual_hlp_swap_safe(
        market,
        MarketAsset::Quote,
        start_prices,
        end_prices,
        quote_residual_on_entry,
    )
}

/// Mirrors the hLP-deposit admission sequence in the instruction after live
/// mint-supply reconciliation. Keeping this test-only avoids restoring a
/// one-call production wrapper.
fn prepare_hlp_deposit_like_instruction(
    market: &mut Market,
    target_asset: MarketAsset,
    current_slot: u64,
) -> Result<()> {
    market.assert_current_version()?;
    market.accrue_interest_to_slot(current_slot)?;
    if market.base_side.reserves.live_reserve > 0 && market.quote_side.reserves.live_reserve > 0 {
        market.advance_amm_clock(current_slot)?;
        market.checkpoint_hlp_vaults()?;
        let prices = current_hlp_curve_prices(market)?;
        let entry = current_hlp_entry_state_with_prices(market, target_asset, prices)?;
        require!(entry.disposition.admits_entry(), ErrorCode::HlpSettlementUnavailable);
        if market.has_active_hlp()
            && market.amm.ramp.active
            && (!market.amm.applied_curve_parameters.is_cpmm() || !market.amm.ramp.target.is_cpmm())
        {
            let desired = market.amm.desired_curve_parameters(&market.config.amm, current_slot);
            require!(
                desired == market.amm.applied_curve_parameters,
                ErrorCode::HlpSettlementUnavailable
            );
        }
        market.observe_current_risk(current_slot)?;
    }
    Ok(())
}

fn valid_config() -> MarketConfig {
    MarketConfig {
        swap_fee_bps: 30,
        divergence_fee_share_cap_bps: 0,
        volatility_fee_share_cap_bps: 0,
        target_hlp_leverage_bps: BPS_DENOMINATOR * 2,
        settlement_divergence_bps: 500,
        ema_half_life_ms: 60_000,
        directional_ema_half_life_ms: 60_000,
        q_ema_half_life_ms: 60_000,
        max_daily_borrow_bps: 2_000,
        global_health_contribution_cap_bps: 15_000,
        borrow_market_health_floor_bps: 11_000,
        amm: Default::default(),
        irm: Default::default(),
        start_time: 0,
    }
}

fn seeded_market() -> Market {
    let base_mint = Pubkey::new_unique();
    let quote_mint = Pubkey::new_unique();
    let mut base_side = MarketSide {
        asset_mint: base_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    base_side.reserves.live_reserve = 1_000;
    base_side.reserves.cash_reserve = 1_000;
    base_side.shares.ylp_supply = 1_000;

    let mut quote_side = MarketSide {
        asset_mint: quote_mint,
        asset_decimals: 0,
        ..MarketSide::default()
    };
    quote_side.reserves.live_reserve = 2_000;
    quote_side.reserves.cash_reserve = 2_000;
    quote_side.shares.ylp_supply = 1_000;

    let mut base_hlp_vault = HlpVault::default();
    base_hlp_vault.initialize(Pubkey::new_unique());
    let mut quote_hlp_vault = HlpVault::default();
    quote_hlp_vault.initialize(Pubkey::new_unique());

    Market {
        version: MARKET_LAYOUT_VERSION,
        ylp_mint: Pubkey::new_unique(),
        base_side,
        quote_side,
        config: valid_config(),
        amm: Default::default(),
        debt: Debt {
            base_borrow_index_nad: NAD as u128,
            quote_borrow_index_nad: NAD as u128,
            ..Debt::default()
        },
        base_hlp_vault,
        quote_hlp_vault,
        risk: Risk::default(),
        insurance: Insurance::default(),
        params_hash: [7; 32],
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

fn market_with_symmetric_unpaid_interest() -> Market {
    let mut market = seeded_market();
    market.base_side.reserves.live_reserve = 1_100;
    market.base_side.reserves.cash_reserve = 900;
    market.quote_side.reserves.live_reserve = 1_100;
    market.quote_side.reserves.cash_reserve = 900;
    market.debt.base_borrow_index_nad = 2 * NAD as u128;
    market.debt.quote_borrow_index_nad = 2 * NAD as u128;
    market.debt.fixed_base_shares = 100;
    market.debt.fixed_quote_shares = 100;
    market.debt.fixed_base_principal = 100;
    market.debt.fixed_quote_principal = 100;

    assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), 1_000);
    assert_eq!(market.curve_reserve(MarketAsset::Quote).unwrap(), 1_000);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    market
}

#[test]
fn open_hlp_keeps_leverage_debt_on_aggregate_vault() {
    let mut market = seeded_market();

    let receipt = market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();

    assert_eq!(receipt.borrowed_amount, 200);
    assert_eq!(receipt.ylp_amount, 100);
    assert_eq!(receipt.hlp_amount, 100);
    assert_eq!(market.debt.fixed_quote_shares, 0);
    assert!(market.base_hlp_vault.debt_shares > 0);
    assert_eq!(market.base_hlp_vault.debt_principal, 200);
    assert_eq!(market.quote_side.daily_limits.borrowed_bucket, 200);
    assert_eq!(market.base_side.daily_limits.borrowed_bucket, 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 100);
    assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 200);
    assert_eq!(market.base_side.reserves.cash_reserve, 1_100);
    assert_eq!(market.quote_side.reserves.cash_reserve, 2_000);
    assert_eq!(market.base_hlp_vault.last_nav_nad, 100 * NAD as u128);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn direct_hlp_deposit_records_borrow_on_the_opposite_side_in_both_directions() {
    let mut market = seeded_market();

    let receipt = market.deposit_single_sided(MarketAsset::Quote, 200, 1, 7).unwrap();

    assert_eq!(receipt.borrowed_amount, 100);
    assert_eq!(market.base_side.daily_limits.borrowed_bucket, 100);
    assert_eq!(market.base_side.daily_limits.last_decay_slot, 7);
    assert_eq!(market.quote_side.daily_limits.borrowed_bucket, 0);
}

#[test]
fn direct_hlp_deposit_rejects_when_the_shared_borrow_bucket_is_full() {
    let mut market = seeded_market();
    let borrowed_asset = MarketAsset::Quote;
    let limit = market
        .daily_limit_for_side(borrowed_asset, market.config.max_daily_borrow_bps)
        .unwrap();
    market.side_mut(borrowed_asset).daily_limits.borrowed_bucket = limit;
    let debt_before = market.base_hlp_vault.debt_principal;

    let error = market.deposit_single_sided(MarketAsset::Base, 100, 1, 0).unwrap_err();

    assert_eq!(error, error!(ErrorCode::DailyLimitExceeded));
    assert_eq!(market.side(borrowed_asset).daily_limits.borrowed_bucket, limit);
    assert_eq!(market.base_hlp_vault.debt_principal, debt_before);
}

#[test]
fn open_hlp_requires_borrowed_side_cash_headroom() {
    let mut market = seeded_market();
    market.quote_side.reserves.cash_reserve = 199;

    let err = market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap_err();

    assert_eq!(err, error!(ErrorCode::InsufficientBorrowHeadroom));
}

#[test]
fn repeated_open_hlp_mints_against_delta_nav() {
    let mut market = seeded_market();

    let first = market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    let second = market
        .deposit_single_sided(MarketAsset::Base, 120, 1, market.risk.last_snapshot_slot)
        .unwrap();

    assert_eq!(first.hlp_amount, 100);
    assert_eq!(second.hlp_amount, 120);
    assert_eq!(market.base_hlp_vault.hlp_supply, 220);
    assert_eq!(market.base_hlp_vault.ylp_shares, 220);
    assert_eq!(market.base_hlp_vault.last_nav_nad, 220 * NAD as u128);
}

#[test]
fn repeated_cpmm_hlp_open_admits_only_non_worsening_controller_granularity() {
    let mut market = seeded_market();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    market.base_side.reserves.live_reserve = 100_000;
    market.base_side.reserves.cash_reserve = 100_000;
    market.quote_side.reserves.live_reserve = 200_000;
    market.quote_side.reserves.cash_reserve = 200_000;
    market.base_side.shares.ylp_supply = 141_421;
    market.quote_side.shares.ylp_supply = 141_421;

    market
        .deposit_single_sided(MarketAsset::Base, 5_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    assert_eq!(market.base_hlp_vault.residual_exposure, -1_000);
    assert_eq!(market.base_hlp_vault.last_nav_nad, 4_998_500);
    let reference = market.base_hlp_vault.cached_settlement_price_nad;
    let before =
        current_hlp_entry_state_with_prices(&market, MarketAsset::Base, current_hlp_curve_prices(&market).unwrap())
            .unwrap();
    assert_eq!(before.disposition, HlpEntryDisposition::ControllerGranularityLimited);

    // This is the on-chain ordering: update/checkpoint admission runs
    // before the transferred amount is applied to the aggregate vault.
    prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Base, 1).unwrap();
    let receipt = market
        .deposit_single_sided(MarketAsset::Base, 6_000, 1, market.risk.last_snapshot_slot)
        .unwrap();

    assert_eq!(receipt.hlp_amount, 6_001);
    assert_eq!(market.base_hlp_vault.hlp_supply, 11_001);
    assert_eq!(market.base_hlp_vault.ylp_shares, 15_556);
    assert_eq!(market.base_hlp_vault.residual_exposure, -1_000);
    assert_eq!(market.base_hlp_vault.last_nav_nad, 10_998_500);
    assert_eq!(market.base_hlp_vault.cached_settlement_price_nad, reference);
    let after =
        current_hlp_entry_state_with_prices(&market, MarketAsset::Base, current_hlp_curve_prices(&market).unwrap())
            .unwrap();
    assert_eq!(after.disposition, HlpEntryDisposition::ControllerGranularityLimited);
    assert_eq!(
        after.residual_exposure.unsigned_abs(),
        before.residual_exposure.unsigned_abs()
    );
    assert!(after.nav_nad > before.nav_nad);
}

#[test]
fn h_lp_nav_values_collateral_and_debt_in_target_numeraire() {
    let mut market = seeded_market();

    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();

    assert_eq!(
        hlp_collateral_value_nad(&market, MarketAsset::Base, &market.base_hlp_vault).unwrap(),
        200 * NAD as u128
    );
    assert_eq!(
        hlp_debt_value_nad(&market, MarketAsset::Base).unwrap(),
        100 * NAD as u128
    );
    assert_eq!(hlp_nav_nad(&market, MarketAsset::Base).unwrap(), 100 * NAD as u128);
}

#[test]
fn cpmm_hlp_price_fast_path_matches_prepared_curve_rounding() {
    let mut market = seeded_market();
    for (base_decimals, quote_decimals, base_reserve, quote_reserve) in [
        (6, 6, 100_000_003, 299_999_999),
        (6, 9, 9_876_543_211, 1_234_567_891),
        (9, 0, 4_321_000_007, 7_654_321),
    ] {
        market.base_side.asset_decimals = base_decimals;
        market.quote_side.asset_decimals = quote_decimals;
        market.base_side.reserves.live_reserve = base_reserve;
        market.base_side.reserves.cash_reserve = base_reserve;
        market.quote_side.reserves.live_reserve = quote_reserve;
        market.quote_side.reserves.cash_reserve = quote_reserve;

        let fast = current_hlp_curve_prices(&market).unwrap();
        let prepared =
            hlp_curve_prices_from_base_price_nad(market.curve_marginal_price_nad(curve_slot(&market)).unwrap() as u128)
                .unwrap();
        assert_eq!(fast, prepared);
    }
}

#[test]
fn accrued_interest_grows_hlp_debt_and_reduces_nav() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    let debt_before = hlp_debt_value_nad(&market, MarketAsset::Base).unwrap();
    let nav_before = hlp_nav_nad(&market, MarketAsset::Base).unwrap();

    // Simulate 10% borrow-interest accrual on the quote index. The base-hLP
    // borrows quote, so its funding debt grows and its NAV falls. hLP
    // funding interest does not grow virtual reserves because the hLP live
    // component is tracked separately from cash-backed debt.
    market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100;

    let debt_after = hlp_debt_value_nad(&market, MarketAsset::Base).unwrap();
    let nav_after = hlp_nav_nad(&market, MarketAsset::Base).unwrap();
    assert!(debt_after > debt_before);
    assert!(nav_after < nav_before);
    assert_eq!(market.base_hlp_vault.debt_principal, 200);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn unrealized_lending_interest_does_not_inflate_hlp_executable_inventory() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();

    // First add 100 of cash-backed fixed principal to the executable
    // reserve, then accrue another 100 of unpaid interest into live
    // reserve. Only the principal changes the hLP's curve claim.
    market.debt.fixed_base_shares = 100;
    market.debt.fixed_base_principal = 100;
    market.base_side.reserves.live_reserve += 100;
    let after_principal = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();

    market.debt.base_borrow_index_nad = 2 * NAD as u128;
    market.base_side.reserves.live_reserve += 100;
    let after_interest = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();

    assert_eq!(market.unrealized_interest(MarketAsset::Base).unwrap(), 100);
    assert_eq!(
        after_interest.target_inventory_value_nad,
        after_principal.target_inventory_value_nad
    );
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
}

#[test]
fn hlp_uses_ordinary_ylp_live_basis_and_cannot_capture_existing_interest() {
    let reference = market_with_symmetric_unpaid_interest();
    let ordinary = reference.preview_add_liquidity(100, 100).unwrap();
    assert_eq!(ordinary.ylp_amount, 90);

    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = market_with_symmetric_unpaid_interest();
        let (ylp_amount, hlp_amount, _, _) = match target_asset {
            MarketAsset::Base => deposit_base_hlp(&mut market, 100, 100).unwrap(),
            MarketAsset::Quote => deposit_quote_hlp(&mut market, 100, 100).unwrap(),
        };

        // live=1_100, curve=1_000, supply=1_000, contribution=100+100.
        // Ordinary yLP and hLP must both mint floor(100*1_000/1_100)=90.
        assert_eq!(ylp_amount, ordinary.ylp_amount);
        assert_eq!(ylp_amount, 90);
        assert_eq!(
            ylp_live_underlying_amount(&market, MarketAsset::Base, ylp_amount).unwrap(),
            99
        );
        assert_eq!(
            ylp_live_underlying_amount(&market, MarketAsset::Quote, ylp_amount).unwrap(),
            99
        );

        let close = match target_asset {
            MarketAsset::Base => withdraw_base_hlp(&mut market, hlp_amount).unwrap(),
            MarketAsset::Quote => withdraw_quote_hlp(&mut market, hlp_amount).unwrap(),
        };

        assert!(
            close.target_amount_out <= 100,
            "same-slot hLP round trip extracted incumbent interest: {close:?}"
        );
        assert_eq!(close.debt_repaid, 100);
        assert_eq!(market.unrealized_interest(MarketAsset::Base).unwrap(), 100);
        assert_eq!(market.unrealized_interest(MarketAsset::Quote).unwrap(), 100);
        assert_eq!(market.base_side.shares.ylp_supply, 1_000);
        assert_eq!(market.quote_side.shares.ylp_supply, 1_000);
        market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
        market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    }
}

#[test]
fn close_hlp_burns_vault_ylp_and_repays_vault_debt() {
    let mut market = seeded_market();
    let deposit_receipt = market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();

    let withdraw_receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit_receipt.hlp_amount)
        .unwrap();

    assert_eq!(withdraw_receipt.target_amount_out, 100);
    assert_eq!(withdraw_receipt.debt_repaid, 200);
    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.base_hlp_vault.debt_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_principal, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 0);
    assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
    assert_eq!(market.debt.fixed_quote_shares, 0);
    assert_eq!(market.base_side.reserves.live_reserve, 1_000);
    assert_eq!(market.base_side.reserves.cash_reserve, 1_000);
    assert_eq!(market.quote_side.reserves.live_reserve, 2_000);
    assert_eq!(market.quote_side.reserves.cash_reserve, 2_000);
    assert_eq!(market.base_side.shares.ylp_supply, 1_000);
    assert_eq!(market.quote_side.shares.ylp_supply, 1_000);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn close_hlp_realizes_interest_from_borrowed_side_cash() {
    let mut market = seeded_market();
    let deposit_receipt = market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100;

    let withdraw_receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit_receipt.hlp_amount)
        .unwrap();

    assert_eq!(withdraw_receipt.debt_repaid, 220);
    assert_eq!(withdraw_receipt.interest_paid, 20);
    assert_eq!(market.base_hlp_vault.debt_principal, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
    assert_eq!(market.quote_side.reserves.live_reserve, 1_980);
    assert_eq!(market.quote_side.reserves.cash_reserve, 1_980);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn close_hlp_converts_borrowed_side_surplus_into_target_out() {
    let mut market = seeded_market();
    let deposit_receipt = market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.quote_side.reserves.live_reserve = 2_300;
    market.quote_side.reserves.cash_reserve = 2_100;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let withdraw_receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit_receipt.hlp_amount)
        .unwrap();

    assert!(withdraw_receipt.target_amount_out > 100);
    assert_eq!(withdraw_receipt.debt_repaid, 200);
    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.quote_side.reserves.live_reserve, 2_100);
    assert_eq!(market.quote_side.reserves.cash_reserve, 2_100);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn close_hlp_uses_target_side_value_for_borrowed_side_shortfall() {
    let mut market = seeded_market();
    let deposit_receipt = market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.quote_side.reserves.live_reserve = 2_110;
    market.quote_side.reserves.cash_reserve = 1_910;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let withdraw_receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit_receipt.hlp_amount)
        .unwrap();

    assert!(withdraw_receipt.target_amount_out < 100);
    assert_eq!(withdraw_receipt.debt_repaid, 200);
    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.quote_side.reserves.live_reserve, 1_910);
    assert_eq!(market.quote_side.reserves.cash_reserve, 1_910);
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn concentrated_close_surplus_uses_concentrated_exact_curve_quote() {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    enable_concentrated_curve(&mut market);
    let deposit = market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.quote_side.reserves.live_reserve = 2_420_000;
    market.quote_side.reserves.cash_reserve = 2_220_000;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let ylp_amount = market.base_hlp_vault.ylp_shares;
    let supply = market.base_side.shares.ylp_supply;
    let base_redeemed = proportional(market.base_side.reserves.live_reserve, ylp_amount, supply).unwrap();
    let quote_redeemed = proportional(market.quote_side.reserves.live_reserve, ylp_amount, supply).unwrap();
    let debt_repaid =
        Debt::shares_to_debt(market.base_hlp_vault.debt_shares, market.debt.quote_borrow_index_nad).unwrap() as u64;
    let surplus = quote_redeemed - debt_repaid;
    assert!(surplus > 0);

    let mut post_burn = market.clone();
    let base_curve_burn = ylp_curve_underlying_amount(&market, MarketAsset::Base, ylp_amount).unwrap();
    let quote_curve_burn = ylp_curve_underlying_amount(&market, MarketAsset::Quote, ylp_amount).unwrap();
    post_burn.base_side.reserves.live_reserve -= base_curve_burn;
    post_burn.quote_side.reserves.live_reserve -= quote_curve_burn;
    let concentrated_out = post_burn
        .quote_curve_exact_in(MarketAsset::Quote, surplus, 0)
        .unwrap()
        .amount_out;
    let cpmm_out = calculate_raw_amount_out(
        post_burn.quote_side.reserves.live_reserve,
        post_burn.base_side.reserves.live_reserve,
        surplus,
    )
    .unwrap();
    assert_ne!(
        concentrated_out, cpmm_out,
        "fixture must distinguish CONCENTRATED from CPMM"
    );

    let receipt = market
        .withdraw_single_sided(MarketAsset::Base, deposit.hlp_amount)
        .unwrap();
    assert_eq!(receipt.target_amount_out, base_redeemed + concentrated_out);
}

#[test]
fn open_hlp_rejects_settlement_price_divergence() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();

    market.quote_side.reserves.live_reserve = 4_000;
    market.quote_side.reserves.cash_reserve = 3_800;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let err = market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap_err();

    assert_eq!(err, error!(ErrorCode::HlpSettlementUnavailable));
}

#[test]
fn close_hlp_rejects_settlement_price_divergence() {
    let mut market = seeded_market();
    let receipt = market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();

    market.quote_side.reserves.live_reserve = 4_000;
    market.quote_side.reserves.cash_reserve = 3_800;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let err = market
        .withdraw_single_sided(MarketAsset::Base, receipt.hlp_amount)
        .unwrap_err();

    assert_eq!(err, error!(ErrorCode::HlpSettlementUnavailable));
}

#[test]
fn h_lp_checkpoint_preserves_last_settlement_reference() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    let settlement_reference = market.base_hlp_vault.cached_settlement_price_nad;
    market.quote_side.reserves.live_reserve = 2_080;
    market.quote_side.reserves.cash_reserve = 1_880;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    checkpoint_hlp_vaults(&mut market).unwrap();
    assert_eq!(market.base_hlp_vault.cached_settlement_price_nad, settlement_reference);
    assert_ne!(
        settlement_reference,
        current_settlement_price_nad(&market, MarketAsset::Base).unwrap(),
        "a generic accounting checkpoint must not re-anchor the settlement guard"
    );
}

fn assert_hlp_near_target(market: &Market, target_asset: MarketAsset, max_gap_nad: u128) {
    let gap = current_hlp_ideal_delta(market, target_asset).unwrap();
    assert!(
        gap.unsigned_abs() <= max_gap_nad,
        "hLP target gap {} exceeds {}",
        gap,
        max_gap_nad
    );
}

fn assert_market_hlp_invariants(market: &Market) {
    market.base_side.assert_share_backing().unwrap();
    market.quote_side.assert_share_backing().unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

fn price_diff_bps(before_nad: u64, after_nad: u64) -> u64 {
    if before_nad == 0 {
        return 0;
    }
    before_nad.abs_diff(after_nad).saturating_mul(BPS_DENOMINATOR as u64) / before_nad
}

fn set_side_live_preserving_hlp_invariant(market: &mut Market, asset: MarketAsset, live_reserve: u64) {
    let hlp_live = market.hlp_live_reserve(asset).unwrap() as u64;
    let live_reserve = live_reserve.max(hlp_live + 1);
    let cash_reserve = live_reserve - hlp_live;
    let side = market.side_mut(asset);
    side.reserves.live_reserve = live_reserve;
    side.reserves.cash_reserve = cash_reserve;
    match asset {
        MarketAsset::Base => {
            market.debt.fixed_base_shares = 0;
            market.debt.fixed_base_principal = 0;
        }
        MarketAsset::Quote => {
            market.debt.fixed_quote_shares = 0;
            market.debt.fixed_quote_principal = 0;
        }
    }
}

fn constrain_side_cash_preserving_hlp_invariant(market: &mut Market, asset: MarketAsset, cash_bps: u64) {
    let live_reserve = market.side(asset).reserves.live_reserve;
    let hlp_live = market.hlp_live_reserve(asset).unwrap() as u64;
    let non_hlp_backing = live_reserve.checked_sub(hlp_live).unwrap();
    let cash_reserve = non_hlp_backing
        .checked_mul(cash_bps)
        .unwrap()
        .checked_div(BPS_DENOMINATOR as u64)
        .unwrap();
    let cash_backed_debt = non_hlp_backing.checked_sub(cash_reserve).unwrap();
    market.side_mut(asset).reserves.cash_reserve = cash_reserve;
    match asset {
        MarketAsset::Base => {
            market.debt.fixed_base_shares = cash_backed_debt as u128;
            market.debt.fixed_base_principal = cash_backed_debt;
        }
        MarketAsset::Quote => {
            market.debt.fixed_quote_shares = cash_backed_debt as u128;
            market.debt.fixed_quote_principal = cash_backed_debt;
        }
    }
}

#[derive(Debug)]
struct TestCompositeSwapReceipt {
    amount_out: u64,
    base_pre_rebalance: HlpRebalanceReceipt,
    quote_pre_rebalance: HlpRebalanceReceipt,
    base_rebalance: HlpRebalanceReceipt,
    quote_rebalance: HlpRebalanceReceipt,
}

fn active_hlp_market() -> Market {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market
        .deposit_single_sided(MarketAsset::Quote, 200_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    assert_market_hlp_invariants(&market);
    market
}

fn enable_concentrated_curve(market: &mut Market) {
    market.config.amm = AmmConfig {
        peak_depth_nad: 200 * NAD,
        fade_scale_nad: NAD / 10,
        adjustment_threshold_nad: NAD / 100,
        adjustment_step_nad: NAD / 1_000,
        min_adjustment_interval_slots: 1,
        ..AmmConfig::default()
    };
    market.ensure_amm_initialized(0).unwrap();
    assert!(!market.current_curve_parameters(0).is_cpmm());
}

fn active_concentrated_hlp_market() -> Market {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    enable_concentrated_curve(&mut market);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market
        .deposit_single_sided(MarketAsset::Quote, 200_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    assert_market_hlp_invariants(&market);
    market
}

#[test]
fn concentrated_hlp_guard_rejects_without_mutating_state() {
    let mut market = active_concentrated_hlp_market();
    let reference = u64::try_from(market.base_hlp_vault.cached_settlement_price_nad).unwrap();
    let stale_trade = market.quote_curve_exact_in(MarketAsset::Base, 150_000, 0).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            150_000,
            stale_trade.amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();
    market.checkpoint_amm_neutral_inventory(0).unwrap();
    checkpoint_hlp_vaults(&mut market).unwrap();
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
    let start = market.curve_marginal_price_nad(0).unwrap();
    let end = if start < reference {
        start / 2
    } else {
        start.checked_mul(2).unwrap()
    };
    let base_before = market.base_hlp_vault;
    let quote_before = market.quote_hlp_vault;

    let read_only_error = require_hlp_swap_path_safe(&market, start, end, true, false).unwrap_err();
    assert_eq!(read_only_error, error!(ErrorCode::HlpSettlementUnavailable));
    assert_eq!(market.base_hlp_vault.hlp_supply, base_before.hlp_supply);
    assert_eq!(market.base_hlp_vault.residual_exposure, base_before.residual_exposure);
    assert_eq!(market.base_hlp_vault.last_nav_nad, base_before.last_nav_nad);
    assert_eq!(market.quote_hlp_vault.hlp_supply, quote_before.hlp_supply);
    assert_eq!(market.quote_hlp_vault.residual_exposure, quote_before.residual_exposure);
    assert_eq!(market.quote_hlp_vault.last_nav_nad, quote_before.last_nav_nad);
}

#[test]
fn cpmm_hlp_path_keeps_inside_and_restoring_trades_live_but_rejects_worsening_flow() {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.config.settlement_divergence_bps = 500;
    let reference = u64::try_from(market.base_hlp_vault.cached_settlement_price_nad).unwrap();

    // A fully settled vault may accept a first large move because the inline
    // controller will recompute and apply the maximum-safe correction from the
    // actual post-trade state.
    require_hlp_swap_path_safe(&market, reference, reference * 2, false, false).unwrap();

    // Create an actual post-trade remainder. Once it exists, the old settlement reference
    // admits inside-band and restoring flow but blocks further deterioration.
    let stale_trade = market.quote_curve_exact_in(MarketAsset::Base, 250_000, 0).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            250_000,
            stale_trade.amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();
    checkpoint_hlp_vaults(&mut market).unwrap();
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
    require_hlp_swap_path_safe(&market, reference, reference * 104 / 100, true, false).unwrap();
    require_hlp_swap_path_safe(&market, reference * 110 / 100, reference * 106 / 100, true, false).unwrap();
    assert_eq!(
        require_hlp_swap_path_safe(&market, reference * 106 / 100, reference * 110 / 100, true, false,).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );
}

#[test]
fn concentrated_hlp_checkpoint_records_exposure_without_moving_inventory() {
    let mut market = active_concentrated_hlp_market();
    // First create actual off-center inventory without settling the hLP.
    // The supplied start is farther from the last settlement reference
    // than that executable endpoint, so the later admission is restorative.
    let swap = market.quote_curve_exact_in(MarketAsset::Base, 150_000, 0).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            150_000,
            swap.amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();
    let base_live_before = market.base_side.reserves.live_reserve;
    let base_cash_before = market.base_side.reserves.cash_reserve;
    let quote_live_before = market.quote_side.reserves.live_reserve;
    let quote_cash_before = market.quote_side.reserves.cash_reserve;
    let base_ylp_before = market.base_hlp_vault.ylp_shares;
    let quote_ylp_before = market.quote_hlp_vault.ylp_shares;

    let (base_delta, quote_delta) = checkpoint_hlp_vaults(&mut market).unwrap();

    assert_eq!(market.base_side.reserves.live_reserve, base_live_before);
    assert_eq!(market.base_side.reserves.cash_reserve, base_cash_before);
    assert_eq!(market.quote_side.reserves.live_reserve, quote_live_before);
    assert_eq!(market.quote_side.reserves.cash_reserve, quote_cash_before);
    assert_eq!(market.base_hlp_vault.ylp_shares, base_ylp_before);
    assert_eq!(market.quote_hlp_vault.ylp_shares, quote_ylp_before);
    assert_eq!(base_delta, market.base_hlp_vault.residual_exposure);
    assert_eq!(quote_delta, market.quote_hlp_vault.residual_exposure);
    assert!(base_delta != 0 || quote_delta != 0);
}

fn funded_due_ramp_with_residual_base_hlp() -> (Market, u64) {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    enable_concentrated_curve(&mut market);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1, market.risk.last_snapshot_slot)
        .unwrap();

    let trade = market.quote_curve_exact_in(MarketAsset::Base, 250_000, 0).unwrap();
    market.base_side.credit_reserve(trade.amount_in, true).unwrap();
    market.quote_side.debit_reserve(trade.amount_out, true).unwrap();
    market.checkpoint_amm_neutral_inventory(0).unwrap();

    // Fund the parameter move with retained principal, then checkpoint the
    // incumbent hLP at the still-applied pre-ramp curve.
    let base_retained = market.base_side.reserves.live_reserve / 100;
    let quote_retained = market.quote_side.reserves.live_reserve / 100;
    market.base_side.credit_reserve(base_retained, true).unwrap();
    market.quote_side.credit_reserve(quote_retained, true).unwrap();
    market.checkpoint_amm_retained_surcharge(0).unwrap();
    checkpoint_hlp_vaults(&mut market).unwrap();

    let values = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();
    assert_ne!(hlp_opposite_exposure_nad(values).unwrap(), 0);
    assert!(market.amm.spendable_protected_profit_nad() > 0);

    let applied = market.amm.applied_curve_parameters;
    let mut target = market.config.amm;
    target.peak_depth_nad = 220 * NAD;
    target.fade_scale_nad = 11 * NAD / 100;
    market.amm.start_applied_ramp(applied, &target, 0).unwrap();
    market.config.amm = target;
    let due_slot = market.amm.ramp.end_slot;
    market.debt.base_last_accrual_slot = due_slot;
    market.debt.quote_last_accrual_slot = due_slot;
    (market, due_slot)
}

fn apply_test_composite_swap(
    market: &mut Market,
    asset_in: MarketAsset,
    amount_in_after_fee: u64,
) -> TestCompositeSwapReceipt {
    let (base_pre_rebalance, quote_pre_rebalance) =
        pre_solve_hlp_vaults_for_swap(market, asset_in, amount_in_after_fee).unwrap();
    let pre_solve_ylp_mint_amount = base_pre_rebalance
        .ylp_mint_amount
        .checked_add(quote_pre_rebalance.ylp_mint_amount)
        .unwrap();
    let fee_eligible_ylp_supply = market
        .side(asset_in)
        .shares
        .ylp_supply
        .checked_sub(pre_solve_ylp_mint_amount)
        .unwrap();
    let (market_side_in, market_side_out) = market.swap_sides(asset_in);
    let amount_out = calculate_raw_amount_out(
        market_side_in.reserves.live_reserve,
        market_side_out.reserves.live_reserve,
        amount_in_after_fee,
    )
    .unwrap();
    market
        .swap_reserves_with_fee_supply(
            asset_in,
            amount_in_after_fee,
            amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
            Some(fee_eligible_ylp_supply),
        )
        .unwrap();
    let (base_rebalance, quote_rebalance) = market
        .finalize_hlp_vaults_for_swap(base_pre_rebalance, quote_pre_rebalance, curve_slot(market))
        .unwrap();
    assert_market_hlp_invariants(market);
    TestCompositeSwapReceipt {
        amount_out,
        base_pre_rebalance,
        quote_pre_rebalance,
        base_rebalance,
        quote_rebalance,
    }
}

fn assert_no_hlp_residuals(market: &Market) {
    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_principal, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
    assert_eq!(market.quote_hlp_vault.hlp_supply, 0);
    assert_eq!(market.quote_hlp_vault.ylp_shares, 0);
    assert_eq!(market.quote_hlp_vault.debt_shares, 0);
    assert_eq!(market.quote_hlp_vault.debt_principal, 0);
    assert_eq!(market.quote_hlp_vault.residual_exposure, 0);
    assert_eq!(market.quote_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.quote_hlp_vault.quote_hlp_live_reserve, 0);
    assert_market_hlp_invariants(market);
}

fn configure_market_depth(market: &mut Market, base_reserve: u64, price_bps: u64) {
    let quote_reserve = (base_reserve as u128)
        .checked_mul(price_bps as u128)
        .unwrap()
        .checked_div(BPS_DENOMINATOR as u128)
        .unwrap() as u64;
    market.base_side.reserves.live_reserve = base_reserve;
    market.base_side.reserves.cash_reserve = base_reserve;
    market.base_side.shares.ylp_supply = base_reserve;
    market.quote_side.reserves.live_reserve = quote_reserve;
    market.quote_side.reserves.cash_reserve = quote_reserve;
    market.quote_side.shares.ylp_supply = base_reserve;
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn hlp_rebalance_preserves_virtual_invariant_under_price_and_cash_sweeps(
        target_is_base in any::<bool>(),
        base_reserve in 500_000u64..5_000_000,
        price_bps in 5_000u64..30_000,
        deposit_bps in 100u64..2_000,
        move_bps in 6_500u64..15_000,
        borrowed_cash_bps in 0u64..=10_000,
    ) {
        let target_asset = if target_is_base {
            MarketAsset::Base
        } else {
            MarketAsset::Quote
        };
        let borrowed_asset = target_asset.opposite();
        let mut market = seeded_market();
        configure_market_depth(&mut market, base_reserve, price_bps);
        assert_market_hlp_invariants(&market);

        let target_reserve = market.side(target_asset).reserves.live_reserve;
        let deposit_amount = target_reserve
            .checked_mul(deposit_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u64)
            .unwrap()
            .max(1);
        market.deposit_single_sided(target_asset, deposit_amount, 1, market.risk.last_snapshot_slot)
            .unwrap();
        assert_market_hlp_invariants(&market);

        let moved_live = market
            .side(borrowed_asset)
            .reserves
            .live_reserve
            .checked_mul(move_bps)
            .unwrap()
            .checked_div(BPS_DENOMINATOR as u64)
            .unwrap();
        set_side_live_preserving_hlp_invariant(&mut market, borrowed_asset, moved_live);
        constrain_side_cash_preserving_hlp_invariant(
            &mut market,
            borrowed_asset,
            borrowed_cash_bps,
        );
        assert_market_hlp_invariants(&market);

        let price_before =
            market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
        let (base_receipt, quote_receipt) = rebalance_hlp_vaults(&mut market).unwrap();
        let price_after =
            market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();

        assert_market_hlp_invariants(&market);
        prop_assert_eq!(
            base_receipt.residual_exposure,
            base_receipt.ideal_delta - base_receipt.executed_delta
        );
        prop_assert_eq!(
            quote_receipt.residual_exposure,
            quote_receipt.ideal_delta - quote_receipt.executed_delta
        );
        let target_receipt = if target_asset == MarketAsset::Base {
            base_receipt
        } else {
            quote_receipt
        };
        let post_ideal = current_hlp_ideal_delta(&market, target_asset).unwrap();
        let post_nav = hlp_nav_nad(&market, target_asset).unwrap();
        prop_assert_eq!(
            target_receipt.residual_exposure,
            recognized_hlp_residual_exposure(post_ideal, post_nav)
        );
        prop_assert!(
            price_diff_bps(price_before, price_after) <= 2,
            "hLP rebalance moved spot by more than 2 bps: before {}, after {}, base receipt {:?}, quote receipt {:?}",
            price_before,
            price_after,
            base_receipt,
            quote_receipt
        );
    }
}

#[test]
fn rebalance_hlp_leverages_up_with_balanced_ylp() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let ylp_before = market.base_hlp_vault.ylp_shares;
    let debt_before = market.base_hlp_vault.debt_shares;
    let principal_before = market.base_hlp_vault.debt_principal;

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(base_receipt.ideal_delta > 0);
    assert!(base_receipt.executed_delta > 0);
    assert!(base_receipt.ylp_mint_amount > 0);
    assert_eq!(base_receipt.ylp_burn_amount, 0);
    assert!(market.base_hlp_vault.ylp_shares > ylp_before);
    assert!(market.base_hlp_vault.debt_shares > debt_before);
    assert!(market.base_hlp_vault.debt_principal > principal_before);
    assert!(market.base_hlp_vault.base_hlp_live_reserve > 0);
    assert!(market.base_hlp_vault.quote_hlp_live_reserve > 200);
    assert_eq!(market.base_hlp_vault.residual_exposure, base_receipt.residual_exposure);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    assert_hlp_near_target(&market, MarketAsset::Base, 2 * NAD as u128);
}

#[test]
fn close_hlp_after_rebalance_retires_synthetic_live_reserves() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(base_receipt.ylp_mint_amount > 0);
    assert!(market.base_hlp_vault.base_hlp_live_reserve > 0);
    assert!(market.base_hlp_vault.quote_hlp_live_reserve > 200);

    let hlp_amount = market.base_hlp_vault.hlp_supply;
    market.withdraw_single_sided(MarketAsset::Base, hlp_amount).unwrap();

    assert_eq!(market.base_hlp_vault.hlp_supply, 0);
    assert_eq!(market.base_hlp_vault.ylp_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_shares, 0);
    assert_eq!(market.base_hlp_vault.debt_principal, 0);
    assert_eq!(market.base_hlp_vault.base_hlp_live_reserve, 0);
    assert_eq!(market.base_hlp_vault.quote_hlp_live_reserve, 0);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn rebalance_hlp_leverage_up_stores_residual_exposure_when_borrow_cash_is_constrained() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    let settlement_reference_before = market.base_hlp_vault.cached_settlement_price_nad;
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 50;
    market.debt.fixed_quote_shares = 2_150;
    market.debt.fixed_quote_principal = 2_150;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let ideal_before = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
    assert!(ideal_before > 0);

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(base_receipt.executed_delta > 0);
    assert_ne!(base_receipt.residual_exposure, 0);
    assert_eq!(
        base_receipt.residual_exposure,
        base_receipt.ideal_delta - base_receipt.executed_delta
    );
    let post_ideal = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
    let post_nav = hlp_nav_nad(&market, MarketAsset::Base).unwrap();
    assert_eq!(
        base_receipt.residual_exposure,
        recognized_hlp_residual_exposure(post_ideal, post_nav)
    );
    assert!(base_receipt.debt_delta > 0);
    assert!(base_receipt.debt_delta <= 50);
    assert_eq!(market.base_hlp_vault.residual_exposure, base_receipt.residual_exposure);
    assert_eq!(
        market.base_hlp_vault.cached_settlement_price_nad, settlement_reference_before,
        "partial hedge execution must not ratchet the settlement reference"
    );

    let (retry, _) = rebalance_hlp_vaults(&mut market).unwrap();
    assert_eq!(retry.executed_delta, 0);
    assert_eq!(retry.residual_exposure, base_receipt.residual_exposure);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn recognized_hlp_residual_exposure_enforces_absolute_and_relative_boundaries() {
    let large_nav = 20_000 * NAD as u128;
    assert_eq!(recognized_hlp_residual_exposure(10_000, large_nav), 0);
    assert_eq!(recognized_hlp_residual_exposure(-10_000, large_nav), 0);
    assert_eq!(recognized_hlp_residual_exposure(10_001, large_nav), 10_001);
    assert_eq!(recognized_hlp_residual_exposure(-10_001, large_nav), -10_001);

    let small_nav = 9_999 * HLP_REBALANCE_DUST_NAV_DENOMINATOR;
    assert_eq!(recognized_hlp_residual_exposure(9_999, small_nav), 0);
    assert_eq!(recognized_hlp_residual_exposure(10_000, small_nav), 10_000);
}

#[test]
fn zero_target_claim_is_fail_closed_without_bricking_checkpoint() {
    let mut market = seeded_market();
    market.base_side.shares.ylp_supply = 1_500;
    market.quote_side.shares.ylp_supply = 1_500;
    market.base_hlp_vault.hlp_supply = 1;
    market.base_hlp_vault.ylp_shares = 1;

    let values = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();
    assert_eq!(values.target_inventory_value_nad, 0);
    assert!(values.opposite_inventory_value_nad > 0);

    let (base, quote) = checkpoint_hlp_vaults(&mut market).unwrap();
    assert!(base > 0);
    assert_eq!(quote, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, base);

    let receipt = rebalance_one_hlp(&mut market, MarketAsset::Base, 0).unwrap();
    assert_eq!(receipt.executed_delta, 0);
    assert_eq!(receipt.residual_exposure, base);

    let prices = current_hlp_curve_prices(&market).unwrap();
    let normalized = refresh_hlp_after_rebalance(
        &mut market,
        MarketAsset::Base,
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Base,
            ideal_delta: base + 1,
            executed_delta: 1,
            ylp_burn_amount: 1,
            ..HlpRebalanceReceipt::default()
        },
        prices,
    )
    .unwrap();
    assert_eq!(normalized.ideal_delta, base);
    assert_eq!(normalized.executed_delta, 0);
    assert_eq!(normalized.ylp_burn_amount, 1);

    let error = prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Base, 1).unwrap_err();
    assert_eq!(error, error!(ErrorCode::HlpSettlementUnavailable));
    prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Quote, 1).unwrap();
}

#[test]
fn underwater_zero_claim_vault_cannot_block_global_market_update() {
    let mut market = seeded_market();
    market.base_hlp_vault.hlp_supply = 1;
    market.base_hlp_vault.debt_shares = 1;
    market.base_hlp_vault.debt_principal = 1;

    market.accrue_interest_to_slot(1).unwrap();
    market.advance_amm_clock(1).unwrap();
    market.checkpoint_hlp_vaults().unwrap();
    market.refresh_risk().unwrap();

    assert_eq!(market.base_hlp_vault.last_nav_nad, 0);
    assert!(market.base_hlp_vault.residual_exposure < 0);
    let receipt = rebalance_one_hlp(&mut market, MarketAsset::Base, 1).unwrap();
    assert_eq!(receipt.executed_delta, 0);
    assert_eq!(receipt.residual_exposure, market.base_hlp_vault.residual_exposure);
}

#[test]
fn solvent_zero_target_claim_can_still_exit_fully() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 1, 1, market.risk.last_snapshot_slot)
        .unwrap();

    let moved = apply_test_composite_swap(&mut market, MarketAsset::Quote, 3);
    assert_eq!(moved.amount_out, 1);
    let values = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();
    assert_eq!(values.target_inventory_value_nad, 0);
    assert_eq!(values.opposite_inventory_value_nad, values.debt_value_nad);
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);

    let supply = market.base_hlp_vault.hlp_supply;
    let receipt = market.withdraw_single_sided(MarketAsset::Base, supply).unwrap();
    assert_eq!(receipt.hlp_supply, 0);
    assert_no_hlp_residuals(&market);
}

#[test]
fn full_exit_clears_stale_residual_exposure_for_both_hlp_vaults() {
    for (target_asset, deposit_amount) in [(MarketAsset::Base, 100), (MarketAsset::Quote, 200)] {
        let mut market = seeded_market();
        market
            .deposit_single_sided(target_asset, deposit_amount, 1, market.risk.last_snapshot_slot)
            .unwrap();
        let vault = match target_asset {
            MarketAsset::Base => &mut market.base_hlp_vault,
            MarketAsset::Quote => &mut market.quote_hlp_vault,
        };
        vault.residual_exposure = 123;
        let supply = vault.hlp_supply;

        market.withdraw_single_sided(target_asset, supply).unwrap();

        let vault = match target_asset {
            MarketAsset::Base => &market.base_hlp_vault,
            MarketAsset::Quote => &market.quote_hlp_vault,
        };
        assert_eq!(vault.hlp_supply, 0);
        assert_eq!(vault.residual_exposure, 0);
    }
}

#[test]
fn cpmm_swap_skips_unhedgeable_zero_target_vault_without_freezing() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 1, 1, market.risk.last_snapshot_slot)
        .unwrap();
    apply_test_composite_swap(&mut market, MarketAsset::Quote, 3);
    market.debt.quote_borrow_index_nad = (NAD as u128) * 2;
    checkpoint_hlp_vaults(&mut market).unwrap();
    let residual_exposure = market.base_hlp_vault.residual_exposure;
    assert!(residual_exposure < 0);

    let receipt = apply_test_composite_swap(&mut market, MarketAsset::Base, 1);

    assert_eq!(receipt.base_pre_rebalance.executed_delta, 0);
    assert_eq!(receipt.base_rebalance.executed_delta, 0);
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
}

#[test]
fn post_state_residual_exposure_tracks_high_index_and_coarse_share_rounding_for_both_assets() {
    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = seeded_market();
        market.base_side.shares.ylp_supply = 101;
        market.quote_side.shares.ylp_supply = 101;
        market
            .deposit_single_sided(target_asset, 100, 1, market.risk.last_snapshot_slot)
            .unwrap();
        match target_asset {
            MarketAsset::Base => market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100,
            MarketAsset::Quote => market.debt.base_borrow_index_nad = (NAD as u128) * 110 / 100,
        }

        let (base, quote) = rebalance_hlp_vaults(&mut market).unwrap();
        let receipt = if target_asset == MarketAsset::Base { base } else { quote };
        let post_ideal = current_hlp_ideal_delta(&market, target_asset).unwrap();
        let post_nav = hlp_nav_nad(&market, target_asset).unwrap();
        assert_eq!(
            receipt.residual_exposure,
            recognized_hlp_residual_exposure(post_ideal, post_nav)
        );
        assert_eq!(receipt.executed_delta, receipt.ideal_delta - receipt.residual_exposure);
        assert_eq!(
            match target_asset {
                MarketAsset::Base => market.base_hlp_vault.residual_exposure,
                MarketAsset::Quote => market.quote_hlp_vault.residual_exposure,
            },
            receipt.residual_exposure
        );
    }
}

#[test]
fn rebalance_hlp_leverage_up_keeps_swap_live_without_borrow_cash() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 0;
    market.debt.fixed_quote_shares = 2_200;
    market.debt.fixed_quote_principal = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let ideal_before = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
    assert!(ideal_before > 0);

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert_eq!(base_receipt.executed_delta, 0);
    assert_eq!(base_receipt.residual_exposure, ideal_before);
    assert_eq!(base_receipt.debt_delta, 0);
    assert_eq!(market.base_hlp_vault.residual_exposure, ideal_before);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn rebalance_hlp_leverage_up_keeps_parent_operation_live_when_daily_capacity_is_exhausted() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1, 0).unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let limit = market
        .daily_limit_for_side(MarketAsset::Quote, market.config.max_daily_borrow_bps)
        .unwrap();
    market.quote_side.daily_limits.borrowed_bucket = limit;
    let ideal_before = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
    assert!(ideal_before > 0);

    let receipt = rebalance_one_hlp(&mut market, MarketAsset::Base, 0).unwrap();

    assert_eq!(receipt.executed_delta, 0);
    assert_eq!(receipt.debt_delta, 0);
    assert_eq!(receipt.residual_exposure, ideal_before);
    assert_eq!(market.quote_side.daily_limits.borrowed_bucket, limit);
}

#[test]
fn pre_solve_hlp_cannot_spend_capacity_reserved_for_explicit_leverage_debt() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1, 0).unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let explicit_borrow = 10;
    let limit = market
        .daily_limit_for_side(MarketAsset::Quote, market.config.max_daily_borrow_bps)
        .unwrap();
    market.quote_side.daily_limits.borrowed_bucket = limit - explicit_borrow;

    let receipt =
        rebalance_one_hlp_with_reservation(&mut market, MarketAsset::Base, 0, MarketAsset::Quote, explicit_borrow)
            .unwrap();

    assert_eq!(receipt.executed_delta, 0);
    assert_eq!(receipt.debt_delta, 0);
    assert_ne!(receipt.residual_exposure, 0);
    assert_eq!(market.quote_side.daily_limits.borrowed_bucket, limit - explicit_borrow);
    market
        .record_new_borrow(MarketAsset::Quote, explicit_borrow, 0)
        .unwrap();
    assert_eq!(market.quote_side.daily_limits.borrowed_bucket, limit);
}

#[test]
fn clipped_hlp_dust_does_not_consume_daily_capacity() {
    let mut market = seeded_market();
    market.deposit_single_sided(MarketAsset::Base, 100, 1, 0).unwrap();
    market.quote_side.reserves.live_reserve = 2_400;
    market.quote_side.reserves.cash_reserve = 2_200;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let limit = market
        .daily_limit_for_side(MarketAsset::Quote, market.config.max_daily_borrow_bps)
        .unwrap();
    market.quote_side.daily_limits.borrowed_bucket = limit - 1;

    let receipt = rebalance_one_hlp(&mut market, MarketAsset::Base, 0).unwrap();

    assert!(receipt.ideal_delta > 0);
    assert_eq!(receipt.executed_delta, 0);
    assert_eq!(receipt.ylp_mint_amount, 0);
    assert_eq!(receipt.debt_delta, 0);
    assert_eq!(market.quote_side.daily_limits.borrowed_bucket, limit - 1);
}

#[test]
fn rebalance_hlp_deleverages_with_balanced_ylp() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.quote_side.reserves.live_reserve = 1_800;
    market.quote_side.reserves.cash_reserve = 1_600;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let ylp_before = market.base_hlp_vault.ylp_shares;
    let debt_before = market.base_hlp_vault.debt_shares;
    let principal_before = market.base_hlp_vault.debt_principal;

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(base_receipt.ideal_delta < 0);
    assert!(base_receipt.executed_delta < 0);
    assert!(base_receipt.ylp_burn_amount > 0);
    assert_eq!(base_receipt.ylp_mint_amount, 0);
    assert!(market.base_hlp_vault.ylp_shares < ylp_before);
    assert!(market.base_hlp_vault.debt_shares < debt_before);
    assert!(market.base_hlp_vault.debt_principal < principal_before);
    assert_eq!(market.base_hlp_vault.residual_exposure, base_receipt.residual_exposure);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    assert_hlp_near_target(&market, MarketAsset::Base, 2 * NAD as u128);
}

#[test]
fn rebalance_hlp_deleverage_pays_accrued_interest_from_borrowed_cash() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Base, 100, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.quote_side.reserves.live_reserve = 1_800;
    market.quote_side.reserves.cash_reserve = 1_600;
    market.debt.quote_borrow_index_nad = (NAD as u128) * 110 / 100;
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    let quote_cash_before = market.quote_side.reserves.cash_reserve;
    let principal_before = market.base_hlp_vault.debt_principal;

    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(base_receipt.executed_delta < 0);
    let principal_repaid = principal_before
        .checked_sub(market.base_hlp_vault.debt_principal)
        .unwrap();
    let interest_paid = base_receipt
        .debt_delta
        .unsigned_abs()
        .checked_sub(u128::from(principal_repaid))
        .unwrap();
    assert!(interest_paid > 0);
    assert_eq!(base_receipt.interest_paid as u128, interest_paid);
    assert!(
        quote_cash_before
            .checked_sub(market.quote_side.reserves.cash_reserve)
            .unwrap() as u128
            >= interest_paid
    );
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
}

#[test]
fn quote_hlp_rebalance_moves_both_ylp_sides() {
    let mut market = seeded_market();
    market
        .deposit_single_sided(MarketAsset::Quote, 200, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market.base_side.reserves.live_reserve = 1_200;
    market.base_side.reserves.cash_reserve = 1_100;
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    let ylp_before = market.quote_hlp_vault.ylp_shares;
    let debt_before = market.quote_hlp_vault.debt_shares;
    let principal_before = market.quote_hlp_vault.debt_principal;

    let (_, quote_receipt) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(quote_receipt.ideal_delta > 0);
    assert!(quote_receipt.executed_delta > 0);
    assert!(quote_receipt.ylp_mint_amount > 0);
    assert!(market.quote_hlp_vault.ylp_shares > ylp_before);
    assert!(market.quote_hlp_vault.debt_shares > debt_before);
    assert!(market.quote_hlp_vault.debt_principal > principal_before);
    market.assert_virtual_reserve_invariant(MarketAsset::Base).unwrap();
    market.assert_virtual_reserve_invariant(MarketAsset::Quote).unwrap();
    assert_hlp_near_target(&market, MarketAsset::Quote, 7 * NAD as u128);
}

#[test]
fn swap_rebalance_is_price_neutral_after_user_quote() {
    let mut market = seeded_market();
    market.base_side.reserves.live_reserve = 1_000_000;
    market.base_side.reserves.cash_reserve = 1_000_000;
    market.base_side.shares.ylp_supply = 1_000_000;
    market.quote_side.reserves.live_reserve = 2_000_000;
    market.quote_side.reserves.cash_reserve = 2_000_000;
    market.quote_side.shares.ylp_supply = 1_000_000;

    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market
        .deposit_single_sided(MarketAsset::Quote, 200_000, 1, market.risk.last_snapshot_slot)
        .unwrap();

    let amount_in_after_fee = 50_000;
    let amount_out = calculate_raw_amount_out(
        market.base_side.reserves.live_reserve,
        market.quote_side.reserves.live_reserve,
        amount_in_after_fee,
    )
    .unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            amount_in_after_fee,
            amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();

    let quoted_post_swap_price = market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
    let base_liquidity_before = market.base_side.reserves.live_reserve;
    let quote_liquidity_before = market.quote_side.reserves.live_reserve;

    let (base_receipt, quote_receipt) = rebalance_hlp_vaults(&mut market).unwrap();

    assert!(
        base_receipt.executed_delta != 0 || quote_receipt.executed_delta != 0,
        "test must exercise an hLP rebalance"
    );
    assert_ne!(market.base_side.reserves.live_reserve, base_liquidity_before);
    assert_ne!(market.quote_side.reserves.live_reserve, quote_liquidity_before);

    let post_rebalance_price = market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
    let price_diff = quoted_post_swap_price.abs_diff(post_rebalance_price);
    assert!(
        price_diff <= quoted_post_swap_price / BPS_DENOMINATOR as u64 + 1,
        "hLP rebalance moved post-swap spot by more than rounding: quoted {}, final {}",
        quoted_post_swap_price,
        post_rebalance_price
    );
}

#[test]
fn small_swap_skips_hlp_pre_solve() {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1, market.risk.last_snapshot_slot)
        .unwrap();

    let (base_receipt, quote_receipt) = pre_solve_hlp_vaults_for_swap(&mut market, MarketAsset::Base, 1).unwrap();

    assert_eq!(base_receipt.executed_delta, 0);
    assert_eq!(quote_receipt.executed_delta, 0);
    assert_eq!(base_receipt.ylp_mint_amount, 0);
    assert_eq!(quote_receipt.ylp_mint_amount, 0);
    assert_market_hlp_invariants(&market);
}

#[test]
fn concentrated_swap_uses_current_state_without_cpmm_predictive_changes() {
    let mut market = active_concentrated_hlp_market();
    let base_before = market.base_side.reserves.live_reserve;
    let quote_before = market.quote_side.reserves.live_reserve;

    let (base_receipt, quote_receipt) = pre_solve_hlp_vaults_for_swap(&mut market, MarketAsset::Base, 350_000).unwrap();

    for (receipt, target_asset) in [(base_receipt, MarketAsset::Base), (quote_receipt, MarketAsset::Quote)] {
        assert_eq!(receipt.target_asset, target_asset);
        assert_eq!(receipt.ideal_delta, 0);
        assert_eq!(receipt.executed_delta, 0);
        assert_eq!(receipt.residual_exposure, 0);
        assert_eq!(receipt.current_swap_fee_eligible_ylp_shares, 0);
        assert_eq!(receipt.ylp_mint_amount, 0);
        assert_eq!(receipt.ylp_burn_amount, 0);
        assert_eq!(receipt.debt_delta, 0);
        assert_eq!(receipt.interest_paid, 0);
        assert!(receipt.nav_nad > 0, "an active hLP receipt reports current-state NAV");
    }
    assert_eq!(market.base_side.reserves.live_reserve, base_before);
    assert_eq!(market.quote_side.reserves.live_reserve, quote_before);
}

#[test]
fn concentrated_rebalance_uses_actual_inventory_exposure_and_preserves_curve_price() {
    let mut market = active_concentrated_hlp_market();
    let swap = market.quote_curve_exact_in(MarketAsset::Base, 150_000, 0).unwrap();
    market
        .swap_reserves(
            MarketAsset::Base,
            150_000,
            swap.amount_out,
            0,
            0,
            crate::state::ProtocolAuctionSplit::default(),
        )
        .unwrap();

    let values_before = current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap();
    assert_ne!(
        values_before.target_inventory_value_nad, values_before.opposite_inventory_value_nad,
        "the fixture must leave the 50/50-value CPMM special case"
    );
    let expected = ideal_hlp_rebalance_nad(values_before)
        .unwrap()
        .total_liquidity_value_nad;
    let legacy_two_x = i128::try_from(
        values_before
            .target_inventory_value_nad
            .checked_add(values_before.opposite_inventory_value_nad)
            .unwrap(),
    )
    .unwrap()
        - i128::try_from(values_before.debt_value_nad.checked_mul(2).unwrap()).unwrap();
    assert_eq!(current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap(), expected);
    assert_ne!(expected, legacy_two_x);

    let exposure_before = hlp_opposite_exposure_nad(values_before).unwrap().unsigned_abs();
    let price_before = market.curve_marginal_price_nad(0).unwrap();
    let (base_receipt, _) = rebalance_hlp_vaults(&mut market).unwrap();
    assert_ne!(base_receipt.executed_delta, 0);
    let price_after = market.curve_marginal_price_nad(0).unwrap();
    let exposure_after =
        hlp_opposite_exposure_nad(current_hlp_inventory_values_nad(&market, MarketAsset::Base).unwrap())
            .unwrap()
            .unsigned_abs();

    assert!(
        exposure_after < exposure_before,
        "curve-aware rebalance must reduce opposite exposure: before {}, after {}",
        exposure_before,
        exposure_after
    );
    assert!(
        price_diff_bps(price_before, price_after) <= 2,
        "proportional executable reserve claims moved CONCENTRATED marginal price: before {}, after {}",
        price_before,
        price_after
    );
}

#[test]
fn due_funded_ramp_blocks_new_hlp_deposit_until_a_swap_like_operation_advances_it() {
    let (mut market, due_slot) = funded_due_ramp_with_residual_base_hlp();
    let applied = market.amm.applied_curve_parameters;
    let settlement_reference = market.base_hlp_vault.cached_settlement_price_nad;
    let settlement_reference_u64 = u64::try_from(settlement_reference).unwrap();
    let worsening_start = settlement_reference_u64.checked_mul(106).unwrap() / 100;
    let worsening_end = settlement_reference_u64.checked_mul(110).unwrap() / 100;
    assert_eq!(
        require_hlp_swap_path_safe(&market, worsening_start, worsening_end, true, false,).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );

    let error = prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Base, due_slot).unwrap_err();
    assert_eq!(error, error!(ErrorCode::HlpSettlementUnavailable));
    assert_eq!(market.amm.applied_curve_parameters, applied);

    let exit_amount = (market.base_hlp_vault.hlp_supply / 10).max(1);
    let exit = market.withdraw_single_sided(MarketAsset::Base, exit_amount).unwrap();
    assert!(exit.target_amount_out > 0);
    assert_eq!(market.amm.applied_curve_parameters, applied);
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(
        market.base_hlp_vault.cached_settlement_price_nad, settlement_reference,
        "a partial exit must not ratchet the settlement band around residual inventory"
    );
    assert_eq!(
        require_hlp_swap_path_safe(&market, worsening_start, worsening_end, true, false,).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );
}

#[test]
fn residual_hlp_exposure_cannot_freeze_the_lazy_controller() {
    let (mut market, due_slot) = funded_due_ramp_with_residual_base_hlp();
    let applied = market.amm.applied_curve_parameters;
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);

    market.prepare_amm_for_swap(due_slot).unwrap();
    let moved = market.advance_one_amm_controller_target(due_slot).unwrap();

    assert!(moved);
    assert_ne!(market.amm.applied_curve_parameters, applied);
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
}

#[test]
fn hlp_deposit_refreshes_actual_exposure_before_entry_gate() {
    let mut market = active_concentrated_hlp_market();
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    market.debt.quote_borrow_index_nad = (NAD as u128) * 101 / 100;

    let error = prepare_hlp_deposit_like_instruction(&mut market, MarketAsset::Base, 1).unwrap_err();

    assert_eq!(error, error!(ErrorCode::HlpSettlementUnavailable));
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
}

#[test]
fn the_next_user_operation_refreshes_exposure_without_being_blocked_by_it() {
    let mut market = active_concentrated_hlp_market();
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    let center = market.amm.center_price_nad;
    market.debt.quote_borrow_index_nad = (NAD as u128) * 101 / 100;

    let (base_residual_exposure, quote_residual_exposure) = market.checkpoint_hlp_vaults().unwrap();
    market.prepare_amm_for_swap(1).unwrap();
    let moved = market.advance_one_amm_controller_target(1).unwrap();

    assert!(!moved);
    assert_ne!(base_residual_exposure, 0);
    assert_eq!(quote_residual_exposure, 0);
    assert_ne!(market.base_hlp_vault.residual_exposure, 0);
    assert_eq!(market.amm.center_price_nad, center);
}

#[test]
fn recognized_checkpoint_dust_does_not_reappear_on_the_next_user_operation() {
    let mut market = seeded_market();
    let scale = NAD;
    market.base_side.asset_decimals = 9;
    market.quote_side.asset_decimals = 9;
    market.base_side.reserves.live_reserve *= scale;
    market.base_side.reserves.cash_reserve *= scale;
    market.quote_side.reserves.live_reserve *= scale;
    market.quote_side.reserves.cash_reserve *= scale;
    market.base_side.shares.ylp_supply *= scale;
    market.quote_side.shares.ylp_supply *= scale;
    enable_concentrated_curve(&mut market);
    market
        .deposit_single_sided(MarketAsset::Base, 100 * scale, 1, market.risk.last_snapshot_slot)
        .unwrap();

    market.debt.quote_borrow_index_nad = (NAD + 1) as u128;
    let actual = current_hlp_ideal_delta(&market, MarketAsset::Base).unwrap();
    let nav = hlp_nav_nad(&market, MarketAsset::Base).unwrap();
    assert_ne!(actual, 0);
    assert_eq!(recognized_hlp_residual_exposure(actual, nav), 0);

    let (base, quote) = checkpoint_hlp_vaults(&mut market).unwrap();
    assert_eq!((base, quote), (0, 0));
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
    market.prepare_amm_for_swap(1).unwrap();
    assert!(!market.advance_one_amm_controller_target(1).unwrap());
    let (base, quote) = checkpoint_hlp_vaults(&mut market).unwrap();
    assert_eq!((base, quote), (0, 0));
    assert_eq!(market.base_hlp_vault.residual_exposure, 0);
}

#[test]
fn large_swap_pre_solve_changes_quote_visible_depth() {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market
        .deposit_single_sided(MarketAsset::Base, 100_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    market
        .deposit_single_sided(MarketAsset::Quote, 200_000, 1, market.risk.last_snapshot_slot)
        .unwrap();

    let amount_in_after_fee = 350_000;
    let user_only_out = calculate_raw_amount_out(
        market.base_side.reserves.live_reserve,
        market.quote_side.reserves.live_reserve,
        amount_in_after_fee,
    )
    .unwrap();
    let price_before = market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();

    let (base_receipt, quote_receipt) =
        pre_solve_hlp_vaults_for_swap(&mut market, MarketAsset::Base, amount_in_after_fee).unwrap();

    assert!(
        base_receipt.executed_delta != 0 || quote_receipt.executed_delta != 0,
        "large swap should execute a quote-visible hLP pre-adjustment"
    );
    assert!(
        base_receipt.executed_delta != 0 && quote_receipt.executed_delta != 0,
        "both active hLP vaults should be eligible for pre-adjustment"
    );
    let pre_solved_out = calculate_raw_amount_out(
        market.base_side.reserves.live_reserve,
        market.quote_side.reserves.live_reserve,
        amount_in_after_fee,
    )
    .unwrap();
    assert_ne!(pre_solved_out, user_only_out);

    let price_after = market_spot_price_nad(&market.base_side, &market.quote_side).unwrap();
    assert!(
        price_diff_bps(price_before, price_after) <= 2,
        "pre-adjustment must preserve marginal spot within rounding"
    );
    assert_market_hlp_invariants(&market);
}

#[test]
fn pre_and_post_hlp_settlement_nets_to_one_token_cpi_per_side() {
    let mint_then_burn = combine_hlp_rebalance_receipts(
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Base,
            ylp_mint_amount: 10,
            interest_paid: 2,
            ..HlpRebalanceReceipt::default()
        },
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Base,
            ylp_burn_amount: 4,
            interest_paid: 3,
            ..HlpRebalanceReceipt::default()
        },
    )
    .unwrap();
    assert_eq!(mint_then_burn.ylp_mint_amount, 6);
    assert_eq!(mint_then_burn.ylp_burn_amount, 0);
    assert_eq!(mint_then_burn.interest_paid, 5);

    let burn_then_mint = combine_hlp_rebalance_receipts(
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Quote,
            ylp_burn_amount: 10,
            ..HlpRebalanceReceipt::default()
        },
        HlpRebalanceReceipt {
            target_asset: MarketAsset::Quote,
            ylp_mint_amount: 4,
            ..HlpRebalanceReceipt::default()
        },
    )
    .unwrap();
    assert_eq!(burn_then_mint.ylp_mint_amount, 0);
    assert_eq!(burn_then_mint.ylp_burn_amount, 6);
}

#[test]
fn swap_pre_solve_reaches_the_endogenous_price_fixed_point() {
    let market = active_hlp_market();
    let asset_in = MarketAsset::Quote;
    let amount_in_after_fee = 350_000;

    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        let equity_nad = hlp_nav_nad(&market, target_asset).unwrap();
        let provisional_ratio =
            simulated_swap_price_ratio_nad(&market, target_asset, asset_in, amount_in_after_fee, 0, true).unwrap();
        let (_, lever_up) = closed_form_pre_adjustment_nad(equity_nad, provisional_ratio).unwrap();
        CPMM_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(|count| count.set(0));
        let mut solved_market = market.clone();
        let solved_receipt = pre_solve_one_hlp_for_swap(
            &mut solved_market,
            target_asset,
            asset_in,
            amount_in_after_fee,
            amount_in_after_fee,
            0,
            asset_in,
            0,
        )
        .unwrap();
        assert_eq!(solved_receipt.ideal_delta.is_positive(), lever_up);
        let solved = solved_receipt.ideal_delta.unsigned_abs();
        let evaluations = CPMM_PRE_SOLVE_CANDIDATE_EVALUATIONS.with(Cell::get);
        assert_eq!(evaluations, HLP_PRE_SOLVE_EVALUATIONS);
        let needed = needed_pre_adjustment_nad(
            &market,
            target_asset,
            asset_in,
            amount_in_after_fee,
            equity_nad,
            solved,
            lever_up,
        )
        .unwrap();
        let baseline_tracking_gap = closed_form_pre_adjustment_nad(equity_nad, provisional_ratio).unwrap().0;
        let solved_tracking_gap = solved.abs_diff(needed);
        assert!(
            solved_tracking_gap <= baseline_tracking_gap,
            "predictive correction worsened the hedge gap: baseline {baseline_tracking_gap}, solved {solved_tracking_gap}"
        );

        if solved > 0 {
            let target_atom_nad = normalize_to_nad(1, market.side(target_asset).asset_decimals).unwrap();
            let residual = solved.abs_diff(needed);
            assert!(
                residual <= target_atom_nad
                    || recognized_hlp_residual_exposure(i128::try_from(residual).unwrap(), equity_nad) == 0,
                "accepted pre-solve residual exceeds one target atom and recognized dust: solved {solved}, needed {needed}"
            );
        }
    }
}

#[test]
fn compact_cpmm_simulator_matches_stateful_reference_exactly() {
    let market = active_hlp_market();
    let amount_in_for_quote = 350_000;
    let reserve_input_credit = 375_000;

    for target_asset in [MarketAsset::Base, MarketAsset::Quote] {
        for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
            for lever_up in [false, true] {
                for pre_adjustment_nad in [0, 1_000 * NAD as u128, 10_000 * NAD as u128] {
                    let compact = simulated_swap_price_ratio_with_reserve_input_nad(
                        &market,
                        target_asset,
                        asset_in,
                        amount_in_for_quote,
                        reserve_input_credit,
                        pre_adjustment_nad,
                        lever_up,
                    )
                    .unwrap();
                    let stateful = stateful_simulated_swap_price_ratio_nad(
                        &market,
                        target_asset,
                        asset_in,
                        amount_in_for_quote,
                        reserve_input_credit,
                        pre_adjustment_nad,
                        lever_up,
                    )
                    .unwrap();
                    assert_eq!(
                            compact, stateful,
                            "target={target_asset:?}, input={asset_in:?}, lever_up={lever_up}, adjustment={pre_adjustment_nad}"
                        );
                }
            }
        }
    }
}

#[test]
fn retained_surcharge_coordinate_changes_the_hlp_endpoint() {
    let market = active_hlp_market();
    let quote_input = 350_000;
    let without_retention = simulated_swap_price_ratio_with_reserve_input_nad(
        &market,
        MarketAsset::Base,
        MarketAsset::Base,
        quote_input,
        quote_input,
        0,
        true,
    )
    .unwrap();
    let with_retention = simulated_swap_price_ratio_with_reserve_input_nad(
        &market,
        MarketAsset::Base,
        MarketAsset::Base,
        quote_input,
        quote_input + 25_000,
        0,
        true,
    )
    .unwrap();

    assert!(
        with_retention < without_retention,
        "retained base input must move the final base-in price farther outward"
    );
}

#[test]
fn pre_solve_handles_opposing_hlp_flows_without_order_asymmetry() {
    let mut market = active_hlp_market();
    let amount_in_after_fee = 350_000;

    let (base_receipt, quote_receipt) =
        pre_solve_hlp_vaults_for_swap(&mut market, MarketAsset::Base, amount_in_after_fee).unwrap();

    assert!(
        base_receipt.executed_delta < 0,
        "base hLP should deleverage when a base-in swap moves base down"
    );
    assert!(
        quote_receipt.executed_delta > 0,
        "quote hLP should lever up when a base-in swap moves quote up"
    );
    assert_eq!(
        base_receipt.residual_exposure,
        base_receipt.ideal_delta - base_receipt.executed_delta
    );
    assert_eq!(
        quote_receipt.residual_exposure,
        quote_receipt.ideal_delta - quote_receipt.executed_delta
    );
    assert_market_hlp_invariants(&market);
}

#[test]
fn quote_simulation_matches_composite_swap_execution() {
    let mut quoted_market = active_hlp_market();
    let mut executed_market = active_hlp_market();
    let amount_in_after_fee = 350_000;

    let quoted = apply_test_composite_swap(&mut quoted_market, MarketAsset::Base, amount_in_after_fee);
    let executed = apply_test_composite_swap(&mut executed_market, MarketAsset::Base, amount_in_after_fee);

    assert_eq!(quoted.amount_out, executed.amount_out);
    assert_eq!(
        quoted.base_pre_rebalance.executed_delta,
        executed.base_pre_rebalance.executed_delta
    );
    assert_eq!(
        quoted.quote_pre_rebalance.executed_delta,
        executed.quote_pre_rebalance.executed_delta
    );
    assert_eq!(
        quoted_market.base_side.reserves.live_reserve,
        executed_market.base_side.reserves.live_reserve
    );
    assert_eq!(
        quoted_market.quote_side.reserves.live_reserve,
        executed_market.quote_side.reserves.live_reserve
    );
    assert!(
        quoted.base_rebalance.executed_delta != quoted.base_pre_rebalance.executed_delta
            || quoted.quote_rebalance.executed_delta != quoted.quote_pre_rebalance.executed_delta,
        "test must exercise the post-swap hLP phase too"
    );
}

#[test]
fn price_moving_swap_keeps_both_hlp_vaults_close_to_their_deposit_assets() {
    let mut market = active_hlp_market();
    let base_deposit = 100_000u64;
    let quote_deposit = 200_000u64;

    // Quote-in moves the base price up. Both one-sided vaults need a
    // pre-adjustment for the finite move and a post-adjustment back to 2x.
    let swap = apply_test_composite_swap(&mut market, MarketAsset::Quote, 350_000);
    assert!(swap.base_pre_rebalance.executed_delta > 0);
    assert!(swap.quote_pre_rebalance.executed_delta < 0);
    assert!(swap.base_rebalance.executed_delta > swap.base_pre_rebalance.executed_delta);
    assert!(swap.quote_rebalance.executed_delta < swap.quote_pre_rebalance.executed_delta);

    let mut base_close_market = market.clone();
    let base_close = base_close_market
        .withdraw_single_sided(MarketAsset::Base, base_close_market.base_hlp_vault.hlp_supply)
        .unwrap();

    let mut quote_close_market = market;
    let quote_close = quote_close_market
        .withdraw_single_sided(MarketAsset::Quote, quote_close_market.quote_hlp_vault.hlp_supply)
        .unwrap();

    let base_tracking_error_bps = base_close
        .target_amount_out
        .abs_diff(base_deposit)
        .saturating_mul(BPS_DENOMINATOR as u64)
        / base_deposit;
    let quote_tracking_error_bps = quote_close
        .target_amount_out
        .abs_diff(quote_deposit)
        .saturating_mul(BPS_DENOMINATOR as u64)
        / quote_deposit;

    assert!(
        base_tracking_error_bps <= 5,
        "base hLP redemption drifted {} bps: deposited {}, redeemed {}",
        base_tracking_error_bps,
        base_deposit,
        base_close.target_amount_out
    );
    assert!(
        quote_tracking_error_bps <= 5,
        "quote hLP redemption drifted {} bps: deposited {}, redeemed {}",
        quote_tracking_error_bps,
        quote_deposit,
        quote_close.target_amount_out
    );
}

#[test]
fn swap_round_trip_then_hlp_close_leaves_no_synthetic_residuals() {
    let mut market = active_hlp_market();
    let base_hlp_deposit = 100_000;
    let quote_hlp_deposit = 200_000;

    let first_swap = apply_test_composite_swap(&mut market, MarketAsset::Base, 350_000);
    let _second_swap = apply_test_composite_swap(&mut market, MarketAsset::Quote, first_swap.amount_out);

    let base_hlp_supply = market.base_hlp_vault.hlp_supply;
    let quote_hlp_supply = market.quote_hlp_vault.hlp_supply;
    let base_close = market
        .withdraw_single_sided(MarketAsset::Base, base_hlp_supply)
        .unwrap();
    let quote_close = market
        .withdraw_single_sided(MarketAsset::Quote, quote_hlp_supply)
        .unwrap();

    let initial_value_at_final_spot = market
        .spot_value_in_opposite(MarketAsset::Base, base_hlp_deposit)
        .unwrap()
        .checked_add(quote_hlp_deposit)
        .unwrap();
    let realized_value_at_final_spot = market
        .spot_value_in_opposite(MarketAsset::Base, base_close.target_amount_out)
        .unwrap()
        .checked_add(quote_close.target_amount_out)
        .unwrap();
    assert!(
        realized_value_at_final_spot <= initial_value_at_final_spot + 8,
        "round-trip hLP close should not extract combined value: base {:?}, quote {:?}, realized {}, initial {}",
        base_close,
        quote_close,
        realized_value_at_final_spot,
        initial_value_at_final_spot
    );
    assert_no_hlp_residuals(&market);
}

#[test]
fn mass_unwind_is_order_independent_when_cash_is_available() {
    let mut close_first = active_hlp_market();
    let mut ylp_first = active_hlp_market();
    let public_ylp_supply = 1_000_000;

    close_first
        .withdraw_single_sided(MarketAsset::Base, close_first.base_hlp_vault.hlp_supply)
        .unwrap();
    close_first
        .withdraw_single_sided(MarketAsset::Quote, close_first.quote_hlp_vault.hlp_supply)
        .unwrap();
    assert_no_hlp_residuals(&close_first);
    close_first.remove_liquidity(public_ylp_supply).unwrap();
    assert_eq!(close_first.base_side.reserves.live_reserve, 0);
    assert_eq!(close_first.quote_side.reserves.live_reserve, 0);
    assert_eq!(close_first.base_side.shares.ylp_supply, 0);
    assert_eq!(close_first.quote_side.shares.ylp_supply, 0);

    ylp_first.remove_liquidity(public_ylp_supply).unwrap();
    ylp_first
        .withdraw_single_sided(MarketAsset::Base, ylp_first.base_hlp_vault.hlp_supply)
        .unwrap();
    ylp_first
        .withdraw_single_sided(MarketAsset::Quote, ylp_first.quote_hlp_vault.hlp_supply)
        .unwrap();
    assert_no_hlp_residuals(&ylp_first);
    assert_eq!(ylp_first.base_side.reserves.live_reserve, 0);
    assert_eq!(ylp_first.quote_side.reserves.live_reserve, 0);
    assert_eq!(ylp_first.base_side.shares.ylp_supply, 0);
    assert_eq!(ylp_first.quote_side.shares.ylp_supply, 0);
}

#[test]
fn pre_solved_hlp_mints_start_earning_after_current_swap_fee() {
    let mut market = seeded_market();
    configure_market_depth(&mut market, 1_000_000, 20_000);
    market
        .deposit_single_sided(MarketAsset::Quote, 200_000, 1, market.risk.last_snapshot_slot)
        .unwrap();
    let quote_hlp_ylp_before = market.quote_hlp_vault.ylp_shares;

    let amount_in_after_fee = 350_000;
    let (base_receipt, quote_receipt) =
        pre_solve_hlp_vaults_for_swap(&mut market, MarketAsset::Base, amount_in_after_fee).unwrap();
    assert_eq!(base_receipt.ylp_mint_amount, 0);
    assert!(quote_receipt.ylp_mint_amount > 0);
    assert_eq!(quote_receipt.current_swap_fee_eligible_ylp_shares, quote_hlp_ylp_before);
    assert!(quote_receipt.current_swap_fee_eligible_ylp_shares < market.quote_hlp_vault.ylp_shares);

    let pre_solve_minted = base_receipt
        .ylp_mint_amount
        .checked_add(quote_receipt.ylp_mint_amount)
        .unwrap();
    let fee_eligible_supply = market
        .base_side
        .shares
        .ylp_supply
        .checked_sub(pre_solve_minted)
        .unwrap();
    let amount_out = calculate_raw_amount_out(
        market.base_side.reserves.live_reserve,
        market.quote_side.reserves.live_reserve,
        amount_in_after_fee,
    )
    .unwrap();
    market
        .swap_reserves_with_fee_supply(
            MarketAsset::Base,
            amount_in_after_fee,
            amount_out,
            10_000,
            0,
            crate::state::ProtocolAuctionSplit::default(),
            Some(fee_eligible_supply),
        )
        .unwrap();
    checkpoint_hlp_yield_from_ylp_shares(
        &mut market,
        MarketAsset::Quote,
        quote_receipt.current_swap_fee_eligible_ylp_shares,
    )
    .unwrap();
    let growth_after_eligible_checkpoint = market.quote_hlp_vault.base_swap_fee_growth_index_q64;

    checkpoint_hlp_yield_from_ylp(&mut market, MarketAsset::Quote).unwrap();

    assert_eq!(
        market.quote_hlp_vault.base_swap_fee_growth_index_q64,
        growth_after_eligible_checkpoint
    );
}

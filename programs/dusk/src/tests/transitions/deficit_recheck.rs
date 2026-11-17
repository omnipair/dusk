// Regressions from the accrued-interest recheck (2026-11-16, R-1..R-5),
// built on the recheck PoCs.
use super::*;
use crate::state::{AmmState, BorrowPosition};
use crate::transitions::LeverageCollateralFee;

const UNIT: u64 = 1_000_000;

fn amm_market(markup: u16, operator_share: u16) -> Market {
    let mut market = Market::current_layout();
    market.base_side.asset_decimals = 6;
    market.quote_side.asset_decimals = 6;
    market.protocol_interest_markup_bps = markup;
    market.operator.interest_share_bps = operator_share;
    market.debt.base_borrow_index_nad = NAD as u128;
    market.debt.quote_borrow_index_nad = NAD as u128;
    market.add_liquidity(1_000_000 * UNIT, 1_000_000 * UNIT).unwrap();
    market.config.max_daily_borrow_bps = crate::state::MAX_DAILY_BORROW_BPS;
    *market.amm = AmmState::default();
    market.prepare_amm_for_swap(1).unwrap();
    market.risk.base_price_ema_nad = NAD;
    market.risk.quote_price_ema_nad = NAD;
    market.observe_current_risk(1).unwrap();
    market
}

fn accrue(market: &mut Market, asset: MarketAsset, bps: u128) {
    let before = market.public_unpaid_interest(asset).unwrap();
    match asset {
        MarketAsset::Base => market.debt.base_borrow_index_nad = market.debt.base_borrow_index_nad * (10_000 + bps) / 10_000,
        MarketAsset::Quote => market.debt.quote_borrow_index_nad = market.debt.quote_borrow_index_nad * (10_000 + bps) / 10_000,
    }
    let accrued = market.public_unpaid_interest(asset).unwrap() - before;
    market.side_mut(asset).reserves.live_reserve += accrued;
    market.recognize_accrued_interest(asset).unwrap();
}

/// Zero-recovery write-off of a fixed position's `asset` debt (the kernel
/// of `settle_flash_recovery`).
fn default_position(market: &mut Market, position: &mut BorrowPosition, asset: MarketAsset) {
    let (shares, principal) = match asset {
        MarketAsset::Base => (position.fixed_base_shares, position.fixed_base_principal),
        MarketAsset::Quote => (position.fixed_quote_shares, position.fixed_quote_principal),
    };
    let removed = market.debt.fixed_debt_reduction_for_shares(asset, shares).unwrap();
    match asset {
        MarketAsset::Base => {
            market.debt.fixed_base_shares -= shares;
            market.debt.fixed_base_principal -= principal;
            position.fixed_base_shares = 0;
            position.fixed_base_principal = 0;
        }
        MarketAsset::Quote => {
            market.debt.fixed_quote_shares -= shares;
            market.debt.fixed_quote_principal -= principal;
            position.fixed_quote_shares = 0;
            position.fixed_quote_principal = 0;
        }
    }
    position.quote_collateral = 0;
    position.base_collateral = 0;
    market.reconcile_global_health_contribution(position, asset, 0).unwrap();
    market.settle_public_debt(asset, 0, 0, removed).unwrap();
}

/// Borrower A borrows most of `asset`'s cash, 150% interest accrues and A
/// defaults with no recovery: `asset` records a deficit.
fn wipe(market: &mut Market, asset: MarketAsset) {
    let mut a = BorrowPosition::default();
    a.clear_liquidation_auction();
    market.deposit_collateral(&mut a, asset.opposite(), 3_000_000 * UNIT).unwrap();
    market.borrow(&mut a, asset, 200_000 * UNIT, 0, 1).unwrap();
    for _ in 0..2 {
        market.side_mut(asset).daily_borrow_bucket = Default::default();
        market.borrow(&mut a, asset, 175_000 * UNIT, 0, 1).unwrap();
    }
    accrue(market, asset, 15_000);
    default_position(market, &mut a, asset);
    assert!(market.side(asset).fees.ledger.deficit > 0);
}

fn wipe_quote(market: &mut Market) {
    wipe(market, MarketAsset::Quote);
}

fn is_error(result: Result<()>, code: ErrorCode) -> bool {
    matches!(result, Err(error) if error == error!(code))
}

/// AI-001 recheck: exits run on a wiped market with the ledger asserted;
/// trading, deposits and borrowing refuse.
#[test]
fn recheck_exits_after_deficit_keep_the_ledger() {
    use crate::transitions::amm::SwapRequest;
    let mut market = amm_market(0, 0);
    let mut b = BorrowPosition::default();
    b.clear_liquidation_auction();
    market.deposit_collateral(&mut b, MarketAsset::Base, 400_000 * UNIT).unwrap();
    market.borrow(&mut b, MarketAsset::Quote, 100_000 * UNIT, 0, 1).unwrap();
    wipe_quote(&mut market);
    market.update_at_slot(2).unwrap();
    market.assert_interest_ledgers().unwrap();
    market
        .repay_with_finalization_and_fee(&mut b, MarketAsset::Quote, 10_000 * UNIT, LeverageCollateralFee::default(), Some(2))
        .unwrap();
    market.assert_interest_ledgers().unwrap();
    market.remove_liquidity(10_000 * UNIT).unwrap();
    market.finalize_amm_transition_and_observe_risk(2).unwrap();
    market.assert_interest_ledgers().unwrap();
    market.prepare_leverage_margin_operation(3).unwrap();
    let swap = SwapRequest { current_slot: 3, current_unix_timestamp: 0, asset_in: MarketAsset::Base, reserve_credit: 1_000, protocol_fee_bps: 0 }
        .prepare(&mut market.clone())
        .map(|_| ());
    assert!(is_error(swap, ErrorCode::InsufficientLiquidity));
    assert!(is_error(market.clone().borrow(&mut b.clone(), MarketAsset::Base, 1, 0, 3).map(|_| ()), ErrorCode::InterestDeficit));
    assert!(is_error(market.clone().add_liquidity(1_000, 1_000).map(|_| ()), ErrorCode::InterestDeficit));
}

/// R-1: after a deficit an indebted position cannot withdraw collateral
/// against the frozen price; repay-then-withdraw still works.
#[test]
fn recheck_indebted_withdrawal_refused_after_deficit() {
    let mut market = amm_market(0, 0);
    let mut b = BorrowPosition::default();
    b.clear_liquidation_auction();
    market.deposit_collateral(&mut b, MarketAsset::Base, 1_000 * UNIT).unwrap();
    market.borrow(&mut b, MarketAsset::Quote, 100 * UNIT, 0, 1).unwrap();
    wipe_quote(&mut market);
    market.update_at_slot(2).unwrap();
    for amount in [1, 100 * UNIT] {
        let result = market.clone().withdraw_collateral(&mut b.clone(), MarketAsset::Base, amount, 0).map(|_| ());
        assert!(is_error(result, ErrorCode::InterestDeficit));
    }
    // Repay in full, then the debt-free position withdraws everything.
    let debt = b.fixed_quote_debt(&market.debt).unwrap();
    let debt = u64::try_from(debt).unwrap();
    market
        .repay_with_finalization_and_fee(&mut b, MarketAsset::Quote, debt, LeverageCollateralFee::default(), Some(3))
        .unwrap();
    assert_eq!(b.fixed_quote_shares, 0);
    market.withdraw_collateral(&mut b, MarketAsset::Base, 1_000 * UNIT, 0).unwrap();
    assert_eq!(b.base_collateral, 0);
    market.assert_market_invariants().unwrap();
}

/// R-3: whatever the held amount, the terminal close never reverts and the
/// held interest never becomes a borrowed-asset surplus: what the debt does
/// not need is published to the holders in the asset it was held in.
#[test]
fn recheck_held_interest_never_overpays_the_terminal_close() {
    for bps in [2_000u128, 60_000, 1_600_000, 2_510_000] {
        let mut market = amm_market(2_000, 0);
        market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
        for asset in [MarketAsset::Base, MarketAsset::Quote] {
            let cash = market.side(asset).reserves.cash_reserve;
            let borrowed = cash / 2;
            let shares = Debt::debt_to_shares(borrowed, NAD as u128).unwrap();
            match asset {
                MarketAsset::Base => {
                    market.debt.fixed_base_shares = shares;
                    market.debt.fixed_base_principal = borrowed;
                }
                MarketAsset::Quote => {
                    market.debt.fixed_quote_shares = shares;
                    market.debt.fixed_quote_principal = borrowed;
                }
            }
            market.side_mut(asset).reserves.cash_reserve -= borrowed;
        }
        accrue(&mut market, MarketAsset::Quote, 20_000);
        market.risk.base_price_ema_nad = NAD;
        market.risk.quote_price_ema_nad = NAD;
        market.prepare_amm_for_swap(2).unwrap();
        market.freeze_negative_hlp_vaults().unwrap();
        assert_eq!(market.amm.frozen_hlp_mask & 1, 1);
        let curve = [market.curve_reserve(MarketAsset::Base).unwrap(), market.curve_reserve(MarketAsset::Quote).unwrap()];
        let index = market.base_hlp_vault.base_interest_growth_index_q64;
        accrue(&mut market, MarketAsset::Base, bps);
        let (held, _) = market.base_hlp_vault.held_interest(MarketAsset::Base);
        assert!(held > 0);
        assert_eq!(market.base_hlp_vault.base_interest_growth_index_q64, index);
        assert_eq!(market.curve_reserve(MarketAsset::Base).unwrap(), curve[0]);
        assert_eq!(market.curve_reserve(MarketAsset::Quote).unwrap(), curve[1]);
        let quote_credited = market.quote_side.fees.ledger.credited;
        let plan = market.prepare_terminal_hlp_waterfall(MarketAsset::Base, 2, 0, 0).unwrap();
        let receipt = plan
            .consume(&mut market, u64::MAX)
            .unwrap_or_else(|error| panic!("bps={bps}: {error:?}"));
        // No surplus is credited in the borrowed asset.
        assert!(market.quote_side.fees.ledger.credited <= quote_credited, "bps={bps}");
        assert_eq!(market.base_hlp_vault.held_interest(MarketAsset::Base), (0, 0));
        if receipt.socialized_loss == 0 && receipt.interest_canceled == 0 {
            // Held interest above the debt reached the holders in base.
            assert!(market.base_hlp_vault.base_interest_growth_index_q64 > index, "bps={bps}");
        }
        market.assert_market_invariants().unwrap();
    }
}

/// R-5: after a deficit an hLP exit that needs a curve price refuses with
/// `InterestDeficit`, and the terminal close stays the vault's exit.
#[test]
fn recheck_hlp_exit_after_deficit_refuses_cleanly() {
    let mut market = amm_market(0, 0);
    market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
    wipe_quote(&mut market);
    market.update_at_slot(2).unwrap();
    let supply = market.base_hlp_vault.hlp_supply;
    let exit = market.clone().withdraw_single_sided(MarketAsset::Base, supply / 2).map(|_| ());
    assert!(is_error(exit, ErrorCode::InterestDeficit));
    // Its borrowed asset is the wiped one, so it closes by flash.
    let fee = LeverageCollateralFee::default();
    let quote = market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).unwrap();
    market.begin_hlp_flash_recovery(quote).unwrap();
    market.settle_terminal_hlp_flash(quote, u64::MAX, 2).unwrap();
    market.assert_market_invariants().unwrap();
    // The closed vault's receipts burn.
    market.withdraw_single_sided(MarketAsset::Base, supply).unwrap();
}

/// R-2: after a deficit, a vault the terminal close would accept is frozen
/// at its EMA marks, so its new interest is held for its creditors.
#[test]
fn recheck_insolvent_vault_holds_interest_after_deficit() {
    let mut market = amm_market(0, 0);
    market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
    let cash = market.base_side.reserves.cash_reserve;
    let base_borrowed = cash / 2;
    market.debt.fixed_base_shares = Debt::debt_to_shares(base_borrowed, NAD as u128).unwrap();
    market.debt.fixed_base_principal = base_borrowed;
    market.base_side.reserves.cash_reserve -= base_borrowed;
    wipe_quote(&mut market);
    market.update_at_slot(2).unwrap();
    let fee = LeverageCollateralFee::default();
    assert!(market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).unwrap().full);
    assert_eq!(market.amm.frozen_hlp_mask & 1, 1);
    let index = market.base_hlp_vault.base_interest_growth_index_q64;
    accrue(&mut market, MarketAsset::Base, 1_000);
    market.update_at_slot(3).unwrap();
    assert_eq!(market.base_hlp_vault.base_interest_growth_index_q64, index);
    assert!(market.base_hlp_vault.held_interest(MarketAsset::Base).0 > 0);
    market.assert_interest_ledgers().unwrap();
}

/// A base vault in a market whose quote side was wiped, solvent at the
/// frozen EMA marks (base observed at 3 quote).
fn solvent_vault_in_wiped_market() -> Market {
    let mut market = amm_market(2_000, 0);
    market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
    wipe_quote(&mut market);
    market.risk.base_price_ema_nad = 3 * NAD;
    market.risk.quote_price_ema_nad = NAD / 3;
    market.update_at_slot(2).unwrap();
    assert!(!market.hlp_frozen(MarketAsset::Base));
    market
}

/// Product decision 2026-11-17: in a wiped market anyone may wind down any
/// vault, and a wind-down never raises a deficit. When the vault's borrowed
/// asset is the wiped one the native close (which would settle that debt
/// with target inventory) refuses and the flash close, where the buyer pays
/// the borrowed asset, winds it down instead.
#[test]
fn wind_down_never_raises_a_deficit() {
    let fee = LeverageCollateralFee::default();
    let deficits = |m: &Market| [m.base_side.fees.ledger.deficit, m.quote_side.fees.ledger.deficit];
    // Borrowed asset wiped: native refused, flash accepted, no deficit move.
    let mut market = solvent_vault_in_wiped_market();
    let before = deficits(&market);
    assert!(is_error(
        market.clone().prepare_terminal_hlp_waterfall(MarketAsset::Base, 2, 0, 0).map(|_| ()),
        ErrorCode::InterestDeficit
    ));
    let quote = market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).unwrap();
    market.begin_hlp_flash_recovery(quote).unwrap();
    let settled = market.settle_terminal_hlp_flash(quote, 0, 2).unwrap();
    assert_eq!(settled.loss.principal_written_off + settled.loss.interest_canceled, 0);
    assert_eq!(deficits(&market), before);
    market.assert_market_invariants().unwrap();
    // Target asset wiped: the borrowed asset is healthy and the native close
    // runs, recording no deficit either.
    let mut market = amm_market(2_000, 0);
    market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
    wipe(&mut market, MarketAsset::Base);
    market.update_at_slot(2).unwrap();
    let before = deficits(&market);
    let supply = market.base_hlp_vault.hlp_supply;
    let plan = market.prepare_terminal_hlp_waterfall(MarketAsset::Base, 2, 0, 0).unwrap();
    plan.consume(&mut market, u64::MAX).unwrap();
    market.finalize_amm_socialized_loss_and_observe_risk(2, Default::default()).unwrap();
    assert_eq!(deficits(&market), before);
    market.assert_market_invariants().unwrap();
    market.withdraw_single_sided(MarketAsset::Base, supply).unwrap();
    market.assert_market_invariants().unwrap();
}

/// The flash terminal settlement winds a solvent vault down the same way:
/// the buyer pays for its collateral at the EMA, the debt is settled and
/// the recovery above it is credited to the holders.
#[test]
fn solvent_vault_winds_down_by_flash_in_a_wiped_market() {
    let fee = LeverageCollateralFee::default();
    let mut market = solvent_vault_in_wiped_market();
    let supply = market.base_hlp_vault.hlp_supply;
    let junior = market.tranches.junior_reserve_shares;
    let quote = market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).unwrap();
    assert!(quote.full);
    market.begin_hlp_flash_recovery(quote).unwrap();
    let deficit = market.quote_side.fees.ledger.deficit;
    let settled = market.settle_terminal_hlp_flash(quote, 0, 2).unwrap();
    assert_eq!(market.quote_side.fees.ledger.deficit, deficit);
    assert_eq!(settled.loss.principal_written_off, 0);
    assert_eq!(settled.loss.interest_canceled, 0);
    assert!(settled.surplus > 0);
    assert_eq!(market.tranches.junior_reserve_shares, junior);
    market.assert_market_invariants().unwrap();
    market.withdraw_single_sided(MarketAsset::Base, supply).unwrap();
    market.assert_market_invariants().unwrap();
}

/// Outside a wiped market the terminal close keeps its insolvency gate.
#[test]
fn solvent_vault_cannot_be_wound_down_in_a_healthy_market() {
    let fee = LeverageCollateralFee::default();
    let mut market = amm_market(2_000, 0);
    market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
    market.update_at_slot(2).unwrap();
    assert!(is_error(
        market.clone().prepare_terminal_hlp_waterfall(MarketAsset::Base, 2, 0, 0).map(|_| ()),
        ErrorCode::HlpNotLiquidatable
    ));
    assert!(is_error(
        market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).map(|_| ()),
        ErrorCode::HlpNotLiquidatable
    ));
}

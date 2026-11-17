// Backstops that must not fail on interest accounting: price-free exits
// after a recorded deficit (audit AI-001) and terminal hLP closes on a
// drained borrowed asset (audit AI-002).
use super::*;
use crate::{
    state::AmmState,
    transitions::{
        flash_liquidation::LiquidationPositionRef, flash_settlement::LiquidationPositionMut,
        LeverageCollateralFee,
    },
};

const UNIT: u64 = 1_000_000;

/// A two-sided 1M/1M AMM market with an initialized concentrated curve and
/// observed risk.
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

/// What `accrue_side` does after installing a new quote index: live grows by
/// the accrued public interest, then recognition credits it.
fn accrue_quote(market: &mut Market, bps: u128) {
    let before = market.public_unpaid_interest(MarketAsset::Quote).unwrap();
    market.debt.quote_borrow_index_nad = market.debt.quote_borrow_index_nad * (10_000 + bps) / 10_000;
    let accrued = market.public_unpaid_interest(MarketAsset::Quote).unwrap() - before;
    market.quote_side.reserves.live_reserve += accrued;
    market.recognize_accrued_interest(MarketAsset::Quote).unwrap();
}

fn borrower(market: &mut Market, collateral: u64, borrows: &[u64]) -> BorrowPosition {
    let mut position = BorrowPosition::default();
    position.clear_liquidation_auction();
    market
        .deposit_collateral(&mut position, MarketAsset::Base, collateral)
        .unwrap();
    for &amount in borrows {
        // Each borrow lands on a fresh day's bucket.
        market.quote_side.daily_borrow_bucket = Default::default();
        market.borrow(&mut position, MarketAsset::Quote, amount, 0, 1).unwrap();
    }
    position
}

/// Borrower A takes most of the quote cash, 150% interest accrues and A
/// defaults with no recovery through the flash-liquidation settlement: the
/// credited interest exceeds every quote LP atom, so the write-off records
/// a deficit. B and C are healthy quote borrowers.
fn wiped_market() -> (Market, BorrowPosition, BorrowPosition) {
    let mut market = amm_market(2_000, 1_000);
    let b = borrower(&mut market, 400_000 * UNIT, &[100_000 * UNIT]);
    let c = borrower(&mut market, 60_000 * UNIT, &[20_000 * UNIT]);
    let mut a = borrower(
        &mut market,
        3_000_000 * UNIT,
        &[200_000 * UNIT, 175_000 * UNIT, 175_000 * UNIT],
    );
    accrue_quote(&mut market, 15_000);
    market.update_at_slot(1).unwrap();
    let shares = u128::from(a.fixed_quote_shares);
    let allocation = market
        .liquidation_debt_allocation(&LiquidationPositionRef::Borrow(&a), MarketAsset::Quote, shares)
        .unwrap();
    let collateral = a.base_collateral;
    market
        .settle_flash_recovery(
            &mut LiquidationPositionMut::Borrow(&mut a),
            MarketAsset::Quote,
            allocation,
            collateral,
            0,
            true,
            2,
        )
        .expect("the write-off that records the deficit settles");
    assert!(market.quote_side.fees.ledger.deficit > 0);
    assert_eq!(market.curve_reserve(MarketAsset::Quote).unwrap(), 0);
    assert!(market.curve_reserve(MarketAsset::Base).unwrap() > 0);
    market.assert_market_invariants().unwrap();
    (market, b, c)
}

/// AI-001: after a deficit every price-free exit still runs through the
/// prelude and finalizer its instruction uses; only swaps, new credit and
/// new capital refuse.
#[test]
fn deficit_keeps_price_free_exits_open() {
    let (market, b, c) = wiped_market();
    let slot = 3;

    // Every instruction prelude: `update` (harvest, collateral, liquidation),
    // the leverage repayment prelude and the borrow-protection snapshot.
    let mut prelude = market.clone();
    prelude.update_at_slot(slot).expect("update prelude");
    prelude.prepare_leverage_margin_operation(slot).expect("leverage margin prelude");
    prelude
        .prepare_position_protection_snapshot(slot)
        .expect("protection snapshot");
    prelude.refresh_risk_at_slot(slot).expect("risk refresh");
    prelude.current_risk_at_slot(slot).expect("risk read");
    prelude.assert_market_invariants().unwrap();

    // Interest keeps accruing on the frozen debt and is credited to holders.
    let mut accrued = market.clone();
    let credited = accrued.quote_side.fees.ledger.credited;
    accrue_quote(&mut accrued, 100);
    accrued.update_at_slot(slot).unwrap();
    assert!(accrued.quote_side.fees.ledger.credited > credited);
    accrued.assert_market_invariants().unwrap();

    // Harvest and the operator and referral claims pay from remaining cash,
    // first come.
    let mut claims = market.clone();
    claims.update_at_slot(slot).unwrap();
    let cash = claims.quote_side.reserves.cash_reserve;
    assert!(cash > 0);
    assert_eq!(claims.quote_side.pay_interest(cash / 2).unwrap(), cash / 2);
    claims.assert_market_invariants().unwrap();

    // Repayment: the borrower's debt shrinks, the paid interest stays in cash.
    let mut repay = market.clone();
    let mut b_repay = b.clone();
    repay.update_at_slot(slot).unwrap();
    let receipt = repay
        .repay_with_finalization_and_fee(
            &mut b_repay,
            MarketAsset::Quote,
            10_000 * UNIT,
            LeverageCollateralFee::default(),
            Some(slot),
        )
        .expect("repay");
    assert_eq!(receipt.cash_repaid, 10_000 * UNIT);
    repay.assert_market_invariants().unwrap();

    // Collateral can always be added. An indebted position cannot withdraw
    // against the frozen price; repaid in full, it withdraws everything.
    let mut collateral = market.clone();
    let mut b_collateral = b.clone();
    collateral.update_at_slot(slot).unwrap();
    collateral
        .deposit_collateral(&mut b_collateral, MarketAsset::Base, 200_000 * UNIT)
        .expect("deposit collateral");
    assert!(matches!(
        collateral.clone().withdraw_collateral(&mut b_collateral.clone(), MarketAsset::Base, 1, 0),
        Err(error) if error == error!(ErrorCode::InterestDeficit)
    ));
    let debt = u64::try_from(b_collateral.fixed_quote_debt(&collateral.debt).unwrap()).unwrap();
    collateral
        .repay_with_finalization_and_fee(
            &mut b_collateral,
            MarketAsset::Quote,
            debt,
            LeverageCollateralFee::default(),
            Some(slot),
        )
        .expect("repay in full");
    let all = b_collateral.base_collateral;
    collateral
        .withdraw_collateral(&mut b_collateral, MarketAsset::Base, all, 0)
        .expect("withdraw collateral");
    collateral.assert_market_invariants().unwrap();

    // srLP exit: the healthy asset's principal is paid, the wiped asset's is
    // zero.
    let mut exit = market.clone();
    exit.update_at_slot(slot).unwrap();
    let removed = exit.remove_liquidity(10_000 * UNIT).expect("remove liquidity");
    exit.finalize_amm_transition_and_observe_risk(slot)
        .expect("liquidity finalizer");
    assert!(removed.base_amount_out > 0);
    assert_eq!(removed.quote_amount_out, 0);
    exit.assert_market_health_with_fees(Default::default())
        .expect("liquidity exit health check");
    exit.assert_market_invariants().unwrap();

    // Flash liquidation of a position underwater at the last observed price.
    let mut liquidation = market.clone();
    let mut c_liquidated = c.clone();
    liquidation.update_at_slot(slot).unwrap();
    liquidation.risk.base_price_ema_nad = NAD / 10;
    liquidation.risk.directional_base_price_ema_nad = NAD / 10;
    liquidation.risk.quote_price_ema_nad = 10 * NAD;
    liquidation.risk.directional_quote_price_ema_nad = 10 * NAD;
    let fee = LeverageCollateralFee::default();
    let quote = liquidation
        .quote_flash_liquidation(
            &LiquidationPositionRef::Borrow(&c_liquidated),
            MarketAsset::Quote,
            u64::MAX,
            true,
            fee,
            fee,
            fee,
            0,
            slot,
        )
        .expect("flash liquidation quote");
    let allocation = liquidation
        .liquidation_debt_allocation(
            &LiquidationPositionRef::Borrow(&c_liquidated),
            MarketAsset::Quote,
            u128::from(c_liquidated.fixed_quote_shares),
        )
        .unwrap();
    liquidation
        .settle_flash_recovery(
            &mut LiquidationPositionMut::Borrow(&mut c_liquidated),
            MarketAsset::Quote,
            allocation,
            quote.collateral_debit,
            quote.repayment,
            true,
            slot,
        )
        .expect("flash liquidation settlement");
    assert_eq!(c_liquidated.fixed_quote_shares, 0);
    liquidation.assert_market_invariants().unwrap();

    // Refusals: swaps (no curve), new credit and new capital.
    for asset_in in [MarketAsset::Base, MarketAsset::Quote] {
        let mut swap = market.clone();
        let request = crate::transitions::amm::SwapRequest {
            current_slot: slot,
            current_unix_timestamp: 0,
            asset_in,
            reserve_credit: 1_000 * UNIT,
            protocol_fee_bps: 0,
        };
        assert!(matches!(
            request.prepare(&mut swap).map(|_| ()),
            Err(error) if error == error!(ErrorCode::InsufficientLiquidity)
        ));
    }
    let mut refused = market.clone();
    let mut b_refused = b.clone();
    for result in [
        refused.add_liquidity(1_000 * UNIT, 1_000 * UNIT).map(|_| ()),
        refused
            .deposit_single_sided(MarketAsset::Base, 1_000 * UNIT, 0)
            .map(|_| ()),
        refused
            .borrow(&mut b_refused, MarketAsset::Base, 1_000 * UNIT, 0, slot)
            .map(|_| ()),
    ] {
        assert!(matches!(result, Err(error) if error == error!(ErrorCode::InterestDeficit)));
    }
}

/// An insolvent base hLP vault (it borrows quote) whose quote funding has
/// tripled, in a market whose quote cash public borrowers drained to
/// `cash_left`.
fn insolvent_vault_market(markup: u16, cash_left: u64) -> Market {
    let mut market = amm_market(markup, 0);
    market.deposit_single_sided(MarketAsset::Base, 20_000 * UNIT, 1).unwrap();
    let cash = market.quote_side.reserves.cash_reserve;
    let borrowed = cash - cash_left;
    market.debt.fixed_quote_shares = Debt::debt_to_shares(borrowed, market.debt.quote_borrow_index_nad).unwrap();
    market.debt.fixed_quote_principal = borrowed;
    market.quote_side.reserves.cash_reserve -= borrowed;
    let before = market.public_unpaid_interest(MarketAsset::Quote).unwrap();
    market.debt.quote_borrow_index_nad = 3 * NAD as u128;
    let accrued = market.public_unpaid_interest(MarketAsset::Quote).unwrap() - before;
    market.quote_side.reserves.live_reserve += accrued;
    market.recognize_accrued_interest(MarketAsset::Quote).unwrap();
    market.risk.base_price_ema_nad = NAD;
    market.risk.quote_price_ema_nad = NAD;
    market.prepare_amm_for_swap(2).unwrap();
    market.freeze_negative_hlp_vaults().unwrap();
    assert_eq!(market.amm.frozen_hlp_mask & 1, 1);
    market
}

/// AI-002: the native terminal close of an insolvent vault never fails on
/// borrowed-asset cash. The bounty is capped at cash, the protocol share
/// takes what is left, and the absorbed loss is what actually left cash.
#[test]
fn terminal_hlp_close_never_fails_on_drained_borrowed_cash() {
    for markup in [0_u16, 2_000] {
        for cash_left in [0_u64, 1, 50_000_000, 400_000 * UNIT] {
            let mut market = insolvent_vault_market(markup, cash_left);
            let cash = market.quote_side.reserves.cash_reserve;
            let plan = market.prepare_terminal_hlp_waterfall(MarketAsset::Base, 2, 0, 0).unwrap();
            let receipt = plan
                .consume(&mut market, u64::MAX)
                .unwrap_or_else(|error| panic!("markup={markup} cash={cash_left}: {error:?}"));
            assert!(receipt.interest_paid > 0);
            let full_bounty = receipt.interest_paid * crate::constants::HLP_TERMINAL_CALLER_BPS as u64
                / crate::constants::BPS_DENOMINATOR as u64;
            assert_eq!(receipt.caller_bounty, full_bounty.min(cash));
            assert!(receipt.protocol_paid <= cash - receipt.caller_bounty);
            assert_eq!(
                market.quote_side.reserves.cash_reserve,
                cash - receipt.caller_bounty - receipt.protocol_paid
            );
            if markup == 0 {
                assert_eq!(receipt.protocol_paid, 0);
            } else if cash_left == 400_000 * UNIT {
                assert!(receipt.protocol_paid > 0);
            }
            market.assert_market_invariants().unwrap();
        }
    }
}

/// AI-002, flash variant: a full flash close settles into drained borrowed
/// cash at any markup. The vault's borrowed claim tracks its principal, so
/// the funding paid never exceeds the buyer's credit and the credit covers
/// the protocol share; the settlement also takes that share only as far as
/// cash allows.
#[test]
fn terminal_hlp_flash_never_fails_on_drained_borrowed_cash() {
    let fee = LeverageCollateralFee::default();
    for markup in [0_u16, 2_000] {
        for cash_left in [0_u64, 1, 400_000 * UNIT] {
            let mut market = insolvent_vault_market(markup, cash_left);
            let quote = market.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).unwrap();
            assert!(quote.full);
            let cash = market.quote_side.reserves.cash_reserve + quote.reserve_credit;
            market.begin_hlp_flash_recovery(quote).unwrap();
            let settlement = market
                .settle_terminal_hlp_flash(quote, u64::MAX, 2)
                .unwrap_or_else(|error| panic!("markup={markup} cash={cash_left}: {error:?}"));
            assert!(settlement.loss.interest_paid > 0);
            assert!(settlement.base.protocol_paid <= cash);
            assert_eq!(market.quote_side.reserves.cash_reserve, cash - settlement.base.protocol_paid);
            assert!(settlement.loss.interest_paid <= quote.reserve_credit);
            assert_eq!(settlement.base.protocol_paid == 0, markup == 0);
            assert_eq!(market.amm.frozen_hlp_mask & 1, 0);
            market.assert_market_invariants().unwrap();
        }
    }
}


/// AI-004: a write-off found at recognition is absorbed after the same
/// accrual's public interest is credited, so the recorded deficit covers it
/// and the ledger ends with no unrecorded gap.
#[test]
fn recognition_absorbs_write_offs_after_crediting_new_interest() {
    let mut market = amm_market(2_000, 0);
    let _borrower = borrower(&mut market, 3_000_000 * UNIT, &[200_000 * UNIT]);
    // Public interest accrues into the live reserve, not yet credited.
    let before = market.public_unpaid_interest(MarketAsset::Quote).unwrap();
    market.debt.quote_borrow_index_nad *= 2;
    let accrued = market.public_unpaid_interest(MarketAsset::Quote).unwrap() - before;
    market.quote_side.reserves.live_reserve += accrued;
    // Recognized funding that no longer exists, credited to holders and
    // larger than all quote LP capital: its write-off records a deficit.
    let phantom = 2 * u128::from(market.curve_reserve(MarketAsset::Quote).unwrap());
    let ledger = &mut market.quote_side.fees.ledger;
    ledger.funding_recognized += phantom;
    ledger.credited += phantom;
    market.recognize_accrued_interest(MarketAsset::Quote).unwrap();
    assert!(market.quote_side.fees.ledger.deficit > 0);
    assert_eq!(market.curve_reserve(MarketAsset::Quote).unwrap(), 0);
    market.assert_interest_ledger(MarketAsset::Quote).unwrap();
}

/// Cash floor: the protocol share a debt-clearing swap reserves bounds what
/// its public settlement takes, whatever share of the interest leaves with
/// the payment and after a funding payment settled first.
#[test]
fn cash_floor_protocol_share_bounds_the_settlement() {
    use crate::transitions::InterestSource;
    for (unpaid, public, funding) in [
        (1_u128, 3_u128, 0_u128),
        (7, 10, 3),
        (333, 1_000, 1),
        (1_000_003, 9_999_991, 77),
        (5, 5, 5),
    ] {
        let mut ledger = market_ledger();
        ledger.public_recognized = public;
        ledger.funding_recognized = funding;
        ledger.protocol_unpaid = unpaid;
        for paid in [0_u64, 1, (public / 3) as u64, public as u64] {
            let bound = ledger.protocol_share_bound(paid).unwrap();
            for removed in [u128::from(paid), (u128::from(paid) + public) / 2, public] {
                if removed < u128::from(paid) || removed == 0 {
                    continue;
                }
                let mut direct = ledger;
                let (taken, _) = direct.settle(InterestSource::Public, removed, paid).unwrap();
                assert!(taken <= u128::from(bound), "{unpaid}/{public}/{funding} paid={paid} removed={removed}");
                for funding_paid in [1_u64, funding as u64] {
                    if funding_paid == 0 || u128::from(funding_paid) > funding {
                        continue;
                    }
                    let mut after_funding = ledger;
                    after_funding
                        .settle(InterestSource::Funding, u128::from(funding_paid), funding_paid)
                        .unwrap();
                    let (taken, _) = after_funding.settle(InterestSource::Public, removed, paid).unwrap();
                    assert!(taken <= u128::from(bound), "{unpaid}/{public}/{funding} paid={paid} removed={removed} funding={funding_paid}");
                }
            }
        }
    }
}

fn market_ledger() -> crate::state::InterestLedger {
    Market::current_layout().quote_side.fees.ledger
}

/// Public debt of `borrowed` in `asset` from cash, then accrual of `bps` on
/// that asset's index, as `accrue_side` applies it.
fn accrue_public(market: &mut Market, asset: MarketAsset, borrowed: u64, bps: u128) {
    if borrowed > 0 {
        let index = market.debt.borrow_index(asset);
        let shares = Debt::debt_to_shares(borrowed, index).unwrap();
        match asset {
            MarketAsset::Base => {
                market.debt.fixed_base_shares += shares;
                market.debt.fixed_base_principal += borrowed;
            }
            MarketAsset::Quote => {
                market.debt.fixed_quote_shares += shares;
                market.debt.fixed_quote_principal += borrowed;
            }
        }
        market.side_mut(asset).reserves.cash_reserve -= borrowed;
    }
    let before = market.public_unpaid_interest(asset).unwrap();
    match asset {
        MarketAsset::Base => market.debt.base_borrow_index_nad = market.debt.base_borrow_index_nad * (10_000 + bps) / 10_000,
        MarketAsset::Quote => market.debt.quote_borrow_index_nad = market.debt.quote_borrow_index_nad * (10_000 + bps) / 10_000,
    }
    let accrued = market.public_unpaid_interest(asset).unwrap() - before;
    market.side_mut(asset).reserves.live_reserve += accrued;
    market.recognize_accrued_interest(asset).unwrap();
}

/// The frozen base vault of `insolvent_vault_market` after base public
/// interest accrues: its share is held for its creditors.
fn frozen_vault_with_held_interest() -> Market {
    let mut market = insolvent_vault_market(2_000, 400_000 * UNIT);
    accrue_public(&mut market, MarketAsset::Base, 300_000 * UNIT, 3_000);
    market
}

/// Frozen vault: interest credited to its srLP after the freeze is held for
/// its creditors, not published to its holders; interest published before
/// the freeze stays theirs; accrual does not move the curve.
#[test]
fn frozen_vault_holds_its_interest_for_creditors() {
    let mut market = insolvent_vault_market(2_000, 400_000 * UNIT);
    // Base interest accrued before the freeze: unfreeze, accrue, refreeze.
    market.clear_frozen_hlp(MarketAsset::Base).unwrap();
    accrue_public(&mut market, MarketAsset::Base, 300_000 * UNIT, 1_000);
    let published = market.base_hlp_vault.base_interest_growth_index_q64;
    assert!(published > 0);
    market.amm.frozen_hlp_mask |= 1;
    let curve = [MarketAsset::Base, MarketAsset::Quote].map(|asset| market.curve_reserve(asset).unwrap());
    let credited = market.base_side.fees.ledger.credited;
    accrue_public(&mut market, MarketAsset::Base, 0, 2_000);
    let vault = market.base_hlp_vault;
    // Not claimable by the holders: the vault's holder index did not move.
    assert_eq!(vault.base_interest_growth_index_q64, published);
    assert!(vault.held_base_interest > 0);
    // Held in `credited`, so the curve did not move at accrual.
    assert!(market.base_side.fees.ledger.credited - credited >= u128::from(vault.held_base_interest));
    assert_eq!(
        [MarketAsset::Base, MarketAsset::Quote].map(|asset| market.curve_reserve(asset).unwrap()),
        curve
    );
    market.assert_market_invariants().unwrap();
}

/// The terminal close (native and flash) applies the held interest to the
/// debt it cannot otherwise recover: what the pool does not recover falls by
/// exactly the value applied, and the applied interest leaves `credited`.
#[test]
fn terminal_close_applies_held_interest_to_the_vault_loss() {
    let fee = LeverageCollateralFee::default();
    let market = frozen_vault_with_held_interest();
    let held = market.base_hlp_vault.held_base_interest;
    assert!(held > 0);
    // The same vault had its interest gone to its holders instead.
    let mut released = market.clone();
    released.base_hlp_vault.release_held_interest().unwrap();
    let unrecovered = |m: &mut Market| {
        let plan = m.prepare_terminal_hlp_waterfall(MarketAsset::Base, 2, 0, 0).unwrap();
        let receipt = plan.consume(m, u64::MAX).unwrap();
        m.assert_market_invariants().unwrap();
        receipt.socialized_loss + receipt.interest_canceled
    };
    let mut native_released = released.clone();
    let shortfall = unrecovered(&mut native_released);
    let used = market.plan_held_hlp_interest(MarketAsset::Base, shortfall, 2).unwrap();
    assert!(used.value > 0 && used.value <= shortfall);
    let mut native = market.clone();
    assert_eq!(unrecovered(&mut native) + used.value, shortfall);
    assert_eq!(native.base_hlp_vault.held_interest(MarketAsset::Base), (0, 0));
    assert_eq!(
        native.base_side.fees.ledger.credited + u128::from(used.target),
        native_released.base_side.fees.ledger.credited
    );
    let flash_unrecovered = |m: &mut Market| {
        let quote = m.quote_hlp_flash_recovery(MarketAsset::Base, fee, fee).unwrap();
        assert!(quote.full);
        m.begin_hlp_flash_recovery(quote).unwrap();
        let settled = m.settle_terminal_hlp_flash(quote, u64::MAX, 2).unwrap();
        settled.loss.principal_written_off + settled.loss.interest_canceled
    };
    let mut flash_released = released.clone();
    let shortfall = flash_unrecovered(&mut flash_released);
    let used = market.plan_held_hlp_interest(MarketAsset::Base, shortfall, 2).unwrap();
    let mut flash = market.clone();
    assert_eq!(flash_unrecovered(&mut flash) + used.value, shortfall);
    assert_eq!(flash.base_hlp_vault.held_interest(MarketAsset::Base), (0, 0));
    assert_eq!(
        flash.base_side.fees.ledger.credited + u128::from(used.target),
        flash_released.base_side.fees.ledger.credited
    );
}

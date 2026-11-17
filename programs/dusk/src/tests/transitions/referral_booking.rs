// A borrower payment's referral is carved from that payment's protocol
// share, which sits in `protocol_pending` until the instruction books it.
// Each settlement path must leave the share pending through its hLP token
// settlement; booking it earlier without the referral strands the referral.
use crate::state::{LiquidationLossAllocation, ReferralInterestQuote};
use crate::transitions::{flash_liquidation::LiquidationPositionRef as Ref, flash_settlement::LiquidationPositionMut as Mut};

const MARKUP_BPS: u16 = 2_000;
const REFERRAL_SHARE_BPS: u16 = 2_500;

/// The instruction-side booking after a payment: the referral is carved from
/// the debt asset's pending share before anything else books.
fn assert_referral_books(market: &mut Market, debt_asset: MarketAsset, interest_paid: u64, protocol_paid: u64) {
    let split = ProtocolAuctionSplit::default();
    assert!(interest_paid > 0 && protocol_paid > 0, "interest {interest_paid}, protocol {protocol_paid}");
    assert!(market.side(debt_asset).fees.ledger.protocol_pending >= protocol_paid);
    let referral = ReferralInterestQuote::with_protocol_revenue(interest_paid, protocol_paid, Some(REFERRAL_SHARE_BPS))
        .unwrap()
        .referral_amount;
    assert!(referral > 0);
    // Booking without the referral first leaves nothing to carve it from.
    let mut early = market.clone();
    early.book_protocol_interest(split).unwrap();
    assert!(early.book_paid_protocol_interest(debt_asset, split, referral).is_err());
    let before = market.side(debt_asset).fees.referral_interest_liability;
    market.book_paid_protocol_interest(debt_asset, split, referral).unwrap();
    assert_eq!(market.side(debt_asset).fees.referral_interest_liability - before, referral);
    assert_eq!(market.base_side.fees.ledger.protocol_pending, 0);
    assert_eq!(market.quote_side.fees.ledger.protocol_pending, 0);
    market.assert_market_invariants().unwrap();
}

/// A referred leverage position with interest accrued under a protocol
/// markup: principal `principal`, index ×`index_bps`/10,000.
fn referred_position(market: &mut Market, asset: MarketAsset, principal: u64, collateral: u64, index_bps: u128) -> LeveragePosition {
    market.protocol_interest_markup_bps = MARKUP_BPS;
    let mut position = seeded_position(market, asset, principal, collateral);
    position.referral_partner = Pubkey::new_unique();
    position.referral_interest_share_bps = REFERRAL_SHARE_BPS;
    let before = market.debt.isolated_debt(asset).unwrap();
    let index = market.debt.borrow_index(asset) * index_bps / 10_000;
    match asset {
        MarketAsset::Base => market.debt.base_borrow_index_nad = index,
        MarketAsset::Quote => market.debt.quote_borrow_index_nad = index,
    }
    let accrued = market.debt.isolated_debt(asset).unwrap() - before;
    market.side_mut(asset).reserves.live_reserve += accrued;
    market.recognize_accrued_interest(asset).unwrap();
    assert!(market.side(asset).fees.ledger.protocol_unpaid > 0);
    position
}

#[test]
fn close_leverage_leaves_the_payment_share_for_its_referral() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = calibration_market(100_000, 4);
        let mut position = referred_position(&mut market, asset, 10_000_000, 40_000_000, 15_000);
        let prepared = prepare_leverage_swap_with_policy(
            &mut market,
            asset.opposite(),
            position.collateral_amount,
            1,
            SwapCashPolicy::Close {
                debt_asset: asset,
                debt_shares: position.debt_shares,
                debt_principal: position.debt_principal,
                shares_to_burn: position.debt_shares,
            },
        );
        let quote = prepared.leverage_quote();
        let receipt = market
            .close_leverage(&mut position, 0, prepared, full_fee_credit(&quote), 0,
                ProtocolAuctionSplit::default(), 1, LeverageCollateralFee::default())
            .unwrap();
        assert_referral_books(&mut market, asset, receipt.interest_paid, receipt.protocol_paid);
    }
}

#[test]
fn decrease_leverage_leaves_the_payment_share_for_its_referral() {
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = calibration_market(100_000, 4);
        let mut position = referred_position(&mut market, asset, 10_000_000, 40_000_000, 15_000);
        let debit = 8_000_000;
        let mut prepared = prepare_leverage_swap_with_policy(
            &mut market,
            asset.opposite(),
            debit,
            1,
            SwapCashPolicy::Decrease {
                debt_asset: asset,
                debt_shares: position.debt_shares,
                debt_principal: position.debt_principal,
            },
        );
        let quote = prepared.leverage_quote();
        let receipt = market
            .decrease_leverage(&mut position, debit, 0, u64::MAX, &mut prepared, full_fee_credit(&quote), 0,
                ProtocolAuctionSplit::default(), 1, 0, LeverageCollateralFee::default())
            .unwrap();
        assert_referral_books(&mut market, asset, receipt.interest_paid, receipt.protocol_paid);
    }
}

#[test]
fn emergency_liquidation_leaves_the_payment_share_for_its_referral() {
    let fee = LeverageCollateralFee::default();
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = calibration_market(100_000, 4);
        let mut position = referred_position(&mut market, asset, 900_000, 1_000_000, 10_600);
        // A partial sale repays its whole slice, interest included.
        let sold = (1..1_000)
            .map(|step| step * 1_000)
            .find(|&sold| {
                market.clone().prepare_emergency_liquidation(&Ref::Leverage(&position), asset, sold, false,
                    fee, fee, fee, 120, 1, 2_000).is_ok_and(|plan| plan.quote.loss.interest_paid > 0)
            })
            .expect("a partial emergency sale");
        let mut prepared = market
            .prepare_emergency_liquidation(&Ref::Leverage(&position), asset, sold, false, fee, fee, fee, 120, 1, 2_000)
            .unwrap();
        let interest_paid = prepared.quote.loss.interest_paid;
        let (_, protocol_paid) = market
            .apply_emergency_liquidation(&mut Mut::Leverage(&mut position), asset, &mut prepared, 1, 2_000,
                ProtocolAuctionSplit::default())
            .unwrap();
        assert_referral_books(&mut market, asset, interest_paid, protocol_paid);
    }
}

#[test]
fn amm_flash_settlement_leaves_the_payment_share_for_its_referral() {
    let fee = LeverageCollateralFee::default();
    for asset in [MarketAsset::Base, MarketAsset::Quote] {
        let mut market = calibration_market(100_000, 4);
        let position = referred_position(&mut market, asset, 900_000, 1_000_000, 10_600);
        let debt = position.debt_amount(&market.debt).unwrap();
        let settled = [500_u64, 1_000, 2_000, 3_000, 5_000].into_iter().find_map(|bps| {
            let mut trial = market.clone();
            let mut next = position.clone();
            let q = trial
                .quote_flash_liquidation(&Ref::Leverage(&next), asset, debt * bps / 10_000, false, fee, fee, fee, 120, 1)
                .ok()?;
            let allocation = trial.liquidation_debt_allocation(&Ref::Leverage(&next), asset, q.debt_shares_to_burn).ok()?;
            let loss = LiquidationLossAllocation::quote(allocation.principal.min(allocation.debt), allocation.debt,
                allocation.debt).ok()?;
            let mut swap = SwapRequest {
                current_slot: 1,
                current_unix_timestamp: 120,
                asset_in: asset.opposite(),
                reserve_credit: q.collateral_credit,
                protocol_fee_bps: 0,
            }
            .prepare_with_cash_policy(&mut trial, SwapCashPolicy::LiquidationQuote)
            .ok()?;
            let policy = SwapCashPolicy::SettleLiquidation {
                debt_asset: asset,
                isolated: true,
                full: false,
                shares: allocation.shares,
                principal_removed: allocation.principal,
                debt_reduced: allocation.debt,
                repayment: allocation.debt,
            };
            swap.bind_liquidation(&trial, policy).ok()?;
            let credit = full_fee_credit(&swap.leverage_quote());
            let finalized = swap.apply(&mut trial, policy, credit, 1, 0, ProtocolAuctionSplit::default(), None).ok()?;
            trial.apply_liquidation_position(&mut Mut::Leverage(&mut next), asset, allocation, q.collateral_debit, false).ok()?;
            Some((trial, loss.interest_paid, finalized.lifecycle.protocol_paid))
        });
        let (mut settled, interest_paid, protocol_paid) = settled.expect("a partial AMM flash settlement");
        assert_referral_books(&mut settled, asset, interest_paid, protocol_paid);
    }
}

use super::*;
use crate::state::Debt;

fn snapshot() -> (Market, Clock) {
    let bytes = include_bytes!("../fixtures/hlp-entry-devnet-500291305.bin");
    let market = Market::try_deserialize(&mut bytes.as_slice()).unwrap();
    let clock = Clock {
        slot: 500_291_305,
        unix_timestamp: market.config.start_time + 1,
        ..Clock::default()
    };
    (market, clock)
}

#[test]
fn gross_limit_includes_transfer_fee_plateaus_and_caps() {
    assert_eq!(gross_funding_limit(100, |_| Ok(0)).unwrap(), 100);
    // A 1% rounded-up fee: 102 sends 100, 103 sends 101.
    assert_eq!(
        gross_funding_limit(100, |a| Ok(a / 100 + u64::from(a % 100 != 0))).unwrap(),
        102
    );
    assert_eq!(gross_funding_limit(100, |a| Ok(a.min(7))).unwrap(), 107);
    assert_eq!(gross_funding_limit(100, Ok).unwrap(), 0);
    assert_eq!(gross_funding_limit(u64::MAX, |_| Ok(0)).unwrap(), u64::MAX);
}

#[test]
fn native_preview_explains_meta_rejection_and_quotes_usdc_funding() {
    let (mut market, clock) = snapshot();
    let (status, limit) =
        preview_hlp_funding_limit(&mut market, MarketAsset::Base, 10_000_000, false, &clock).unwrap();
    assert_eq!((status, limit), (HlpDepositStatus::RebalanceRequired, 0));
    // This is precisely the deployed transaction guard, not a UI heuristic.
    assert_eq!(
        market.assert_hlp_entry_available(MarketAsset::Base).unwrap_err(),
        error!(ErrorCode::HlpSettlementUnavailable)
    );
    let (mut market, clock) = snapshot();
    let (status, limit) = preview_hlp_funding_limit(&mut market, MarketAsset::Quote, 0, false, &clock).unwrap();
    assert_eq!(status, HlpDepositStatus::Ready);
    assert!(limit > 0);
    let target = market.curve_reserve(MarketAsset::Quote).unwrap() as u128;
    let opposite = market.curve_reserve(MarketAsset::Base).unwrap() as u128;
    let index = market.debt.base_borrow_index_nad;
    let fits = |amount: u64| {
        let borrowed = (amount as u128) * opposite / target;
        let shares = market.quote_hlp_vault.debt_shares + Debt::debt_to_shares(borrowed as u64, index).unwrap();
        Debt::shares_to_debt(shares, index).unwrap() <= market.base_side.reserves.cash_reserve as u128
    };
    assert!(fits(limit));
    assert!(!fits(limit + 1));
    // A real, ordinary-size USDC entry still passes the native transition.
    market.deposit_single_sided(MarketAsset::Quote, 1_000_000, 1).unwrap();
    market.finalize_amm_transition_and_observe_risk(clock.slot).unwrap();
}

#[test]
fn preview_preserves_native_identity_supply_and_market_restrictions() {
    let (mut market, clock) = snapshot();
    assert_eq!(
        preview_hlp_funding_limit(&mut market, MarketAsset::Quote, 0, true, &clock).unwrap(),
        (HlpDepositStatus::ReduceOnly, 0)
    );
    market.reduce_only = true;
    assert_eq!(
        preview_hlp_funding_limit(&mut market, MarketAsset::Quote, 0, false, &clock)
            .unwrap()
            .0,
        HlpDepositStatus::ReduceOnly
    );
    market.reduce_only = false;
    market.config.start_time = clock.unix_timestamp + 1;
    assert_eq!(
        preview_hlp_funding_limit(&mut market, MarketAsset::Quote, 0, false, &clock)
            .unwrap()
            .0,
        HlpDepositStatus::NotStarted
    );
    market.version = 0;
    assert!(preview_hlp_funding_limit(&mut market, MarketAsset::Quote, 0, false, &clock).is_err());
    let (mut market, clock) = snapshot();
    assert!(preview_hlp_funding_limit(&mut market, MarketAsset::Base, 10_000_001, false, &clock).is_err());
    let (mut market, clock) = snapshot();
    assert!(preview_hlp_funding_limit(&mut market, MarketAsset::Base, 0, false, &clock).is_err());
    let (mut market, clock) = snapshot();
    market.base_side.reserves.cash_reserve = 0;
    assert_eq!(
        preview_hlp_funding_limit(&mut market, MarketAsset::Quote, 0, false, &clock)
            .unwrap()
            .0,
        HlpDepositStatus::NoFundingCapacity
    );
}

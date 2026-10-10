use super::*;
use crate::state::Debt;

fn snapshot() -> (Market, Clock) {
    // The captured devnet account predates fractional borrow-index carries.
    // Insert zero carries at the new Debt fields without changing the fixture.
    let mut layout = Market::default();
    layout.debt.base_borrow_index_nad = 0x1234_5678_9abc_def0_1357_2468_ace0_bdf1;
    layout.debt.quote_borrow_index_nad = 0x2468_ace0_1357_bdf1_1234_5678_9abc_def0;
    let mut layout_bytes = Vec::new();
    layout.try_serialize(&mut layout_bytes).unwrap();
    let mut marker = Vec::new();
    marker.extend_from_slice(&layout.debt.base_borrow_index_nad.to_le_bytes());
    marker.extend_from_slice(&layout.debt.quote_borrow_index_nad.to_le_bytes());
    let insertion_offset = layout_bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap()
        + marker.len();
    let mut bytes = include_bytes!("../fixtures/hlp-entry-devnet-500291305.bin").to_vec();
    // The capture also predates per-market liquidation dust amounts. Insert
    // neutral one-atom settings before applying offsets in the current layout.
    let mut dust_layout = Market::default();
    dust_layout.config.liquidation.minimum_base_debt = 0x1357_2468_ace0_bdf1;
    dust_layout.config.liquidation.minimum_quote_debt = 0x2468_1357_bdf1_ace0;
    let mut dust_bytes = Vec::new();
    dust_layout.try_serialize(&mut dust_bytes).unwrap();
    let dust_marker = dust_layout.config.liquidation.try_to_vec().unwrap();
    let dust_offset = dust_bytes.windows(dust_marker.len()).position(|window| window == dust_marker).unwrap();
    bytes.splice(dust_offset..dust_offset, crate::state::LiquidationConfig::default().try_to_vec().unwrap());
    bytes.splice(insertion_offset..insertion_offset, [0u8; 32]);
    // The immutable capture also predates aggregate leverage collateral.
    layout.debt.leverage_base_collateral = 0x1357_9bdf_2468_ace0;
    layout.debt.leverage_quote_collateral = 0x2468_ace0_1357_9bdf;
    layout_bytes.clear();
    layout.try_serialize(&mut layout_bytes).unwrap();
    marker.clear();
    marker.extend_from_slice(&layout.debt.leverage_base_collateral.to_le_bytes());
    marker.extend_from_slice(&layout.debt.leverage_quote_collateral.to_le_bytes());
    let exposure_offset = layout_bytes.windows(marker.len()).position(|window| window == marker).unwrap();
    bytes.splice(exposure_offset..exposure_offset, [0u8; 16]);
    let mut market = Market::try_deserialize(&mut bytes.as_slice()).unwrap();
    market.version = crate::constants::MARKET_LAYOUT_VERSION;
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

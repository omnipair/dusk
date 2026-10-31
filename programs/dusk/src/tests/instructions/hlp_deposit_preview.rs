use super::*;
use crate::state::Debt;

fn snapshot() -> (Market, Clock) {
    // Extend the immutable historical capture with zero-valued fields added
    // since that layout. Locate each insertion in the current serialization and
    // apply ascending offsets, including all fields preceding the Debt block.
    let mut layout = Market::default();
    let tags = [0x3197_5bdf_2468_ace0_u64, 0x4286_ace0_1357_9bdf, 0x5397_ace0_1357_9bdf,
        0x6486_ace0_1357_9bdf, 0x7597_ace0_1357_9bdf, 0x8686_ace0_1357_9bdf,
        0x9797_ace0_1357_9bdf, 0xa886_ace0_1357_9bdf];
    layout.base_side.reserves.flash_receivable = tags[0];
    layout.base_side.fees.public_interest_advance.outstanding_gross = tags[1];
    layout.quote_side.reserves.flash_receivable = tags[2];
    layout.quote_side.fees.public_interest_advance.outstanding_gross = tags[3];
    layout.base_hlp_vault.flash_collateral_debit = tags[4];
    layout.quote_hlp_vault.flash_collateral_debit = tags[5];
    layout.debt.base_borrow_index_remainder = u128::from(tags[6]);
    layout.debt.leverage_base_collateral = tags[7];
    let frozen_offset_tag = 0x3197_5bdf_2468_ace0_4286_ace0_1357_9bdf_i128;
    layout.amm.frozen_base_vault_base_offset_nad = frozen_offset_tag;
    // The historical capture predates the explicit insurance-window flags.
    // Give each start slot a unique marker to locate its preceding flag.
    let insurance_slot_tags = [0xb997_5bdf_2468_ace0_u64, 0xc997_5bdf_2468_ace0_u64];
    layout.insurance.base_draw_window.start_slot = insurance_slot_tags[0];
    layout.insurance.quote_draw_window.start_slot = insurance_slot_tags[1];
    let irm_tag = 0xd997_5bdf_2468_ace0_u64;
    let rate_fraction_tag = 0xe997_5bdf_2468_ace0_u64;
    let tranche_tag = 0xf997_5bdf_2468_ace0_u64;
    layout.config.irm.lower_signal_nad = irm_tag;
    layout.debt.base_rate_fraction_e18 = rate_fraction_tag;
    layout.tranches.junior_reserve_shares = tranche_tag;
    let junior_fee_tags = [0x1197_5bdf_2468_ace0_u64, 0x2297_5bdf_2468_ace0_u64];
    layout.base_side.fees.junior.reserve_shares = junior_fee_tags[0];
    layout.quote_side.fees.junior.reserve_shares = junior_fee_tags[1];
    let mut current = Vec::new();
    layout.try_serialize(&mut current).unwrap();
    let mut insertions: Vec<_> = tags.into_iter().zip([8, 32, 8, 32, 8, 8, 32, 16]).map(|(tag, size)| {
        (current.windows(8).position(|window| window == tag.to_le_bytes()).unwrap(), size)
    }).collect();
    let frozen_offset = current.windows(16).position(|window| window == frozen_offset_tag.to_le_bytes()).unwrap();
    insertions.push((frozen_offset - 1, 65));
    for tag in insurance_slot_tags {
        let offset = current.windows(8).position(|window| window == tag.to_le_bytes()).unwrap();
        insertions.push((offset - 1, 1));
    }
    for (tag, size) in [(rate_fraction_tag, 16), (tranche_tag, 98 + crate::state::MarketIncomeBooks::INIT_SPACE)] {
        let offset = current.windows(8).position(|window| window == tag.to_le_bytes()).unwrap();
        insertions.push((offset, size));
    }
    for tag in junior_fee_tags {
        let offset = current.windows(8).position(|window| window == tag.to_le_bytes()).unwrap() - 1;
        insertions.push((offset, crate::state::TrancheIncome::INIT_SPACE));
    }
    let irm_offset = current.windows(8).position(|window| window == irm_tag.to_le_bytes()).unwrap();
    // Replace the old 18-byte Morpho config with the 50-byte band config.
    let irm = crate::state::IrmConfig::default().try_to_vec().unwrap();
    let mut edits: Vec<_> = insertions.into_iter().map(|(offset, size)| (offset, 0, vec![0u8; size])).collect();
    edits.push((irm_offset, 18, irm));
    edits.sort_by_key(|edit| edit.0);
    let mut bytes = include_bytes!("../fixtures/hlp-entry-devnet-500291305.bin").to_vec();
    for (offset, removed, replacement) in edits { bytes.splice(offset..offset + removed, replacement); }
    let mut market = Market::try_deserialize(&mut bytes.as_slice()).unwrap();
    market.version = crate::constants::MARKET_LAYOUT_VERSION;
    market.junior_exit_config = Default::default();
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
fn native_preview_uses_full_value_hedge_and_quotes_both_funding_limits() {
    let (mut market, clock) = snapshot();
    let (status, limit) =
        preview_hlp_funding_limit(&mut market, MarketAsset::Base, 10_000_000, false, &clock).unwrap();
    assert_eq!(status, HlpDepositStatus::Ready);
    assert!(limit > 0);
    market.assert_hlp_entry_available(MarketAsset::Base).unwrap();
    // Ready reports current hedge/funding headroom. An amount-specific entry
    // still enforces the native post-mint rounding bound.
    assert_eq!(market.deposit_single_sided(MarketAsset::Base, 10_000_000, 1).unwrap_err(), error!(ErrorCode::HlpSettlementUnavailable));
    let (mut market, clock) = snapshot();
    let (status, limit) = preview_hlp_funding_limit(&mut market, MarketAsset::Quote, 0, false, &clock).unwrap();
    assert_eq!(status, HlpDepositStatus::Ready);
    assert!(limit > 0);
    let target = market.quote_side.reserves.live_reserve as u128;
    let opposite = market.base_side.reserves.live_reserve as u128;
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

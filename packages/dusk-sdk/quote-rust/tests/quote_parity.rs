use anchor_lang::prelude::*;
use dusk_native_close_quote::{
    constants, quote_amount_out, quote_debt_amount, quote_last_amount_out, quote_prepare,
    state::{Debt, LeveragePosition, Market, MarketAsset, MarketConfig, MarketSide, ReserveShares, Reserves},
};

#[test]
fn wasm_boundary_uses_serialized_program_state() {
    let mut market = Market::default();
    market.version = constants::MARKET_LAYOUT_VERSION;
    market.base_side = MarketSide {
        reserves: Reserves {
            live_reserve: 2_000_000_000,
            cash_reserve: 2_000_000_000,
            ..Reserves::default()
        },
        shares: ReserveShares {
            ylp_supply: 2_000_000_000,
        },
        ..MarketSide::default()
    };
    market.quote_side = MarketSide {
        reserves: Reserves {
            live_reserve: 2_000_000_000,
            cash_reserve: 2_000_000_000,
            ..Reserves::default()
        },
        shares: ReserveShares {
            ylp_supply: 2_000_000_000,
        },
        ..MarketSide::default()
    };
    market.config = MarketConfig {
        swap_fee_bps: 0,
        divergence_fee_share_cap_bps: 2_000,
        volatility_fee_share_cap_bps: 2_000,
        max_daily_borrow_bps: 3_000,
        ..MarketConfig::default()
    };
    market.debt = Debt {
        base_borrow_index_nad: constants::NAD as u128,
        quote_borrow_index_nad: constants::NAD as u128,
        ..Debt::default()
    };
    let debt = 100_000_000;
    let shares = market.debt.add_isolated_debt(MarketAsset::Base, debt).unwrap();
    market.base_side.reserves.cash_reserve -= debt;
    let position = LeveragePosition {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        namespace_authority: Pubkey::new_unique(),
        referral_partner: Pubkey::default(),
        referral_interest_share_bps: 0,
        debt_asset: 0,
        collateral_amount: 200_000_000,
        margin_amount: debt,
        funded_collateral_amount: debt,
        open_notional: debt * 2,
        debt_principal: debt as u128,
        debt_shares: shares,
        multiplier_bps: 20_000,
        opened_at: 0,
        opened_slot: 0,
        open_curve_revision: 0,
        bump: 1,
    };
    let mut market_bytes = Vec::new();
    let mut position_bytes = Vec::new();
    market.try_serialize(&mut market_bytes).unwrap();
    position.try_serialize(&mut position_bytes).unwrap();
    let code = unsafe {
        quote_prepare(
            market_bytes.as_ptr(),
            market_bytes.len(),
            position_bytes.as_ptr(),
            position_bytes.len(),
            0,
            0,
        )
    };
    assert_eq!(code, 0);
    assert_eq!(quote_debt_amount(), debt);
    assert_eq!(quote_amount_out(1), 2);
    let amount = 110_000_000;
    let status = quote_amount_out(amount);
    assert_eq!(status, 0);
    let expected = quote_last_amount_out();
    assert!(expected > 0);
    let fixture = format!(
        "{{\"market\":\"{}\",\"position\":\"{}\",\"amount\":{},\"output\":{},\"debt\":{}}}",
        hex::encode(market_bytes),
        hex::encode(position_bytes),
        amount,
        expected,
        debt
    );
    let fixture_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/native-close-fixture.json");
    if std::env::var_os("DUSK_UPDATE_NATIVE_CLOSE_FIXTURE").is_some() {
        std::fs::write(fixture_path, &fixture).unwrap();
    } else {
        assert_eq!(std::fs::read_to_string(fixture_path).unwrap(), fixture);
    }
}

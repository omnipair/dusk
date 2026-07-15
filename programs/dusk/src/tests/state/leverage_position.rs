use super::*;
use crate::constants::NAD;

fn blank_position() -> LeveragePosition {
    LeveragePosition {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        position_id: Pubkey::default(),
        debt_asset: 0,
        margin_mode: 0,
        collateral_amount: 0,
        margin_amount: 0,
        open_notional: 0,
        debt_principal: 0,
        debt_shares: 0,
        multiplier_bps: 0,
        opened_at: 0,
        opened_slot: 0,
        bump: 0,
    }
}

#[test]
fn leverage_position_tracks_debt_asset_and_current_debt() {
    let owner = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let mut position = blank_position();
    position.initialize(
        owner,
        market,
        Pubkey::new_unique(),
        MarketAsset::Quote,
        1_000,
        500,
        1_500,
        1_000,
        1_000,
        30_000,
        42,
        7,
        255,
    );
    let debt = Debt {
        quote_borrow_index_nad: (NAD as u128) * 11 / 10,
        ..Debt::default()
    };

    assert!(position.is_initialized());
    assert_eq!(position.debt_asset().unwrap(), MarketAsset::Quote);
    assert_eq!(position.collateral_asset().unwrap(), MarketAsset::Base);
    assert_eq!(position.margin_mode().unwrap(), LeverageMarginMode::Debt);
    assert_eq!(position.margin_asset().unwrap(), MarketAsset::Quote);
    assert_eq!(position.settlement_asset().unwrap(), MarketAsset::Quote);
    assert_eq!(position.debt_amount(&debt).unwrap(), 1_100);
    assert!(position
        .assert_position(owner, market, MarketAsset::Quote)
        .is_ok());
    assert!(position
        .assert_position(owner, market, MarketAsset::Base)
        .is_err());
}

#[test]
fn collateral_margin_mode_uses_collateral_for_margin_and_settlement() {
    let mut position = blank_position();
    position.initialize_with_margin_mode(
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        MarketAsset::Base,
        LeverageMarginMode::Collateral,
        3_000,
        1_000,
        3_000,
        2_000,
        2_000,
        30_000,
        42,
        7,
        255,
    );

    assert_eq!(LeverageMarginMode::Debt.code(), 0);
    assert_eq!(LeverageMarginMode::Collateral.code(), 1);
    assert_eq!(position.margin_mode().unwrap(), LeverageMarginMode::Collateral);
    assert_eq!(position.margin_asset().unwrap(), MarketAsset::Quote);
    assert_eq!(position.settlement_asset().unwrap(), MarketAsset::Quote);
    assert!(LeverageMarginMode::try_from_code(2).is_err());
}

#[test]
fn leverage_position_rejects_invalid_wire_codes_and_margin_mode_mismatch() {
    let mut position = blank_position();
    position.owner = Pubkey::new_unique();
    position.market = Pubkey::new_unique();
    position.debt_shares = 1;
    position.collateral_amount = 1;

    position.margin_mode = 2;
    assert!(position.margin_mode().is_err());
    assert!(position.margin_asset().is_err());
    assert!(position.settlement_asset().is_err());

    position.margin_mode = LeverageMarginMode::Debt.code();
    assert!(position
        .require_margin_mode(LeverageMarginMode::Collateral)
        .is_err());

    position.debt_asset = 2;
    assert!(position.debt_asset().is_err());
    assert!(position.collateral_asset().is_err());
    assert!(position.debt_amount(&Debt::default()).is_err());
}

#[test]
fn leverage_position_open_state_requires_debt_and_collateral() {
    let mut position = blank_position();
    assert!(position.require_open().is_err());

    position.debt_shares = 1;
    assert!(position.require_open().is_err());

    position.collateral_amount = 1;
    assert!(position.require_open().is_ok());

    position.debt_shares = 0;
    assert!(position.require_open().is_err());
}

#[test]
fn leverage_position_collateral_arithmetic_is_checked() {
    let mut position = blank_position();

    assert!(position.credit_collateral(0).is_err());
    position.credit_collateral(10).unwrap();
    assert_eq!(position.collateral_amount, 10);
    position.debit_collateral(4).unwrap();
    assert_eq!(position.collateral_amount, 6);
    assert!(position.debit_collateral(0).is_err());
    assert!(position.debit_collateral(7).is_err());
    assert_eq!(position.collateral_amount, 6);

    position.collateral_amount = u64::MAX;
    assert!(position.credit_collateral(1).is_err());
    assert_eq!(position.collateral_amount, u64::MAX);
}

#[test]
fn leverage_delegation_is_bound_to_owner_market_position_and_asset() {
    let owner = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let position = Pubkey::new_unique();
    let delegated_program = Pubkey::new_unique();
    let mut delegation = LeverageDelegation {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        position: Pubkey::default(),
        debt_asset: 0,
        delegated_program: Pubkey::default(),
        approved_actions: 0,
        bump: 0,
    };
    delegation.initialize(
        owner,
        market,
        position,
        MarketAsset::Quote,
        delegated_program,
        1,
        255,
    );

    assert!(delegation
        .assert_delegation(owner, market, position, MarketAsset::Quote)
        .is_ok());
    assert!(delegation
        .assert_delegation(Pubkey::new_unique(), market, position, MarketAsset::Quote)
        .is_err());
    assert!(delegation
        .assert_delegation(owner, Pubkey::new_unique(), position, MarketAsset::Quote)
        .is_err());
    assert!(delegation
        .assert_delegation(owner, market, Pubkey::new_unique(), MarketAsset::Quote)
        .is_err());
    assert!(delegation
        .assert_delegation(owner, market, position, MarketAsset::Base)
        .is_err());

    let replacement_program = Pubkey::new_unique();
    delegation.update(replacement_program, 7);
    assert_eq!(delegation.delegated_program, replacement_program);
    assert_eq!(delegation.approved_actions, 7);

    delegation.debt_asset = 2;
    assert!(delegation.debt_asset().is_err());
}

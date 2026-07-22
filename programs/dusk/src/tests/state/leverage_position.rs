use super::*;
use crate::constants::NAD;

#[test]
fn leverage_position_tracks_debt_asset_and_current_debt() {
    let owner = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let mut position = LeveragePosition {
        owner: Pubkey::default(),
        market: Pubkey::default(),
        position_id: Pubkey::default(),
        referral_partner: Pubkey::default(),
        referral_interest_share_bps: 0,
        debt_asset: 0,
        collateral_amount: 0,
        margin_amount: 0,
        open_notional: 0,
        debt_principal: 0,
        debt_shares: 0,
        multiplier_bps: 0,
        opened_at: 0,
        opened_slot: 0,
        bump: 0,
    };
    position.initialize(
        owner,
        market,
        Pubkey::new_unique(),
        Pubkey::default(),
        0,
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
    assert_eq!(position.debt_amount(&debt).unwrap(), 1_100);
    assert!(position
        .assert_position(owner, market, MarketAsset::Quote)
        .is_ok());
    assert!(position
        .assert_position(owner, market, MarketAsset::Base)
        .is_err());
}

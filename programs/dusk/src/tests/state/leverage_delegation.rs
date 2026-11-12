use super::*;

#[test]
fn recreated_position_invalidates_prior_delegation_until_owner_updates_it() {
    let owner = Pubkey::new_unique();
    let market = Pubkey::new_unique();
    let position = Pubkey::new_unique();
    let mut delegation = LeverageDelegation {
        owner,
        market,
        position,
        debt_asset: MarketAsset::Base.code(),
        delegated_program: Pubkey::new_unique(),
        approved_actions: 1,
        open_curve_revision: 10,
        bump: 1,
    };
    assert!(delegation
        .assert_delegation(owner, market, position, MarketAsset::Base, 10)
        .is_ok());
    assert!(delegation
        .assert_delegation(owner, market, position, MarketAsset::Base, 11)
        .is_err());
    delegation.open_curve_revision = 11;
    assert!(delegation
        .assert_delegation(owner, market, position, MarketAsset::Base, 11)
        .is_ok());
}

use super::*;

#[test]
fn empty_position_requires_every_terminal_field_to_be_clear() {
    let mut position = BorrowPosition::default();
    position.initialize(Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), 1);
    assert!(position.is_empty());

    position.base_collateral = 1;
    assert!(!position.is_empty());
    position.base_collateral = 0;

    position.fixed_quote_shares = 1;
    assert!(!position.is_empty());
    position.fixed_quote_shares = 0;

    position.set_referral_binding(MarketAsset::Quote, Pubkey::new_unique(), 1);
    assert!(!position.is_empty());
    position.clear_referral_binding(MarketAsset::Quote);

    position.start_liquidation_auction(MarketAsset::Quote, 1, 2, 1);
    assert!(!position.is_empty());
    position.clear_liquidation_auction();

    assert!(position.is_empty());
}

use super::*;

#[test]
fn leverage_entry_funding_vault_is_the_order_ata_for_each_token_program() {
    let order = Pubkey::new_unique();
    let debt_mint = Pubkey::new_unique();
    let legacy = leverage_entry_funding_vault_address(order, debt_mint, Token::id());
    let token_2022 = leverage_entry_funding_vault_address(order, debt_mint, Token2022::id());

    assert_eq!(
        legacy,
        anchor_spl::associated_token::get_associated_token_address_with_program_id(
            &order,
            &debt_mint,
            &Token::id(),
        )
    );
    assert_eq!(
        token_2022,
        anchor_spl::associated_token::get_associated_token_address_with_program_id(
            &order,
            &debt_mint,
            &Token2022::id(),
        )
    );
    assert_ne!(legacy, token_2022);
}

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

/// A 1% transfer fee at creation: a 2,000 deposit credits 1,980, and a net
/// 50 bounty costs 51 gross, leaving 1,929 of margin.
fn entry_order(min_margin_amount: u64) -> LeverageEntryOrder {
    LeverageEntryOrder {
        owner: Pubkey::new_unique(),
        market: Pubkey::new_unique(),
        position: Pubkey::new_unique(),
        position_id: Pubkey::new_unique(),
        debt_mint: Pubkey::new_unique(),
        collateral_mint: Pubkey::new_unique(),
        order_id: 1,
        debt_asset: 1,
        deposit_credit: 1_980,
        margin_amount: 1_929,
        min_margin_amount,
        executor_bounty: 50,
        multiplier_bps: 20_000,
        limit_price_nad: 1,
        min_collateral_out: 1,
        expiry_unix_timestamp: i64::MAX,
        referrer: None,
        bump: 255,
    }
}

#[test]
fn a_transfer_fee_increase_comes_out_of_margin_not_the_bounty() {
    let order = entry_order(1_000);
    // Unchanged fee: the recorded margin.
    assert_eq!(order.execution_margin(51).unwrap(), 1_929);
    // A 10% fee raises the bounty's gross debit to 56; the margin pays it.
    assert_eq!(order.execution_margin(56).unwrap(), 1_924);
    // A lower fee leaves the recorded margin; the saving stays in escrow.
    assert_eq!(order.execution_margin(50).unwrap(), 1_929);
}

#[test]
fn a_margin_below_the_owner_minimum_fails_closed() {
    let order = entry_order(1_925);
    assert_eq!(order.execution_margin(55).unwrap(), 1_925);
    assert_eq!(
        order.execution_margin(56).unwrap_err(),
        error!(LeverageDelegateError::MinimumMarginNotMet)
    );
    // A bounty that alone exceeds the deposit leaves no margin at all.
    assert_eq!(
        entry_order(1).execution_margin(1_981).unwrap_err(),
        error!(LeverageDelegateError::MinimumMarginNotMet)
    );
}

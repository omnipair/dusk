use super::*;

#[test]
fn delegated_payout_caps_fees_at_thin_residual() {
    let mint_key = Pubkey::new_unique();
    let token_program = Token::id();
    let mut lamports = 0;
    let mut data = [];
    let mint = AccountInfo::new(
        &mint_key,
        false,
        false,
        &mut lamports,
        &mut data,
        &token_program,
        false,
        0,
    );
    let payout = quote_delegated_close_payout(&mint, 3, 10_000, 10_000, 0).unwrap();
    assert_eq!(payout.protocol_debit, 3);
    assert_eq!(payout.protocol_credit, 3);
    assert_eq!(payout.executor_debit, 0);
    assert_eq!(payout.owner_credit, 0);

    let payout = quote_delegated_close_payout(&mint, 1_000, 10_000, 10_000, 0).unwrap();
    assert_eq!(payout.protocol_debit, 10);
    assert_eq!(payout.executor_debit, 500);
    assert_eq!(payout.owner_credit, 490);
    assert_eq!(
        payout.protocol_debit + payout.executor_debit + payout.owner_debit,
        1_000
    );
}

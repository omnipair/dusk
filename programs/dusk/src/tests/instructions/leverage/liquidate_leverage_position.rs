use super::*;
use crate::token::get_transfer_fee_for_epoch;
use anchor_lang::solana_program::program_option::COption;
use spl_token_2022::{
    extension::{
        transfer_fee::{TransferFee, TransferFeeConfig},
        BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut,
    },
    state::Mint as SplToken2022Mint,
};

#[test]
fn liquidation_payout_preserves_fee_free_split() {
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
    assert_eq!(liquidation_payout_debits(&mint, 1_000, 100, 0).unwrap(), (100, 900));
    assert_eq!(liquidation_payout_debits(&mint, 1_000, 0, 0).unwrap(), (0, 1_000));
}

#[test]
fn liquidation_payout_grosses_up_net_reward_within_available_proceeds() {
    let mint_len =
        ExtensionType::try_calculate_account_len::<SplToken2022Mint>(&[ExtensionType::TransferFeeConfig]).unwrap();
    let mut mint_data = vec![0_u8; mint_len];
    {
        let mut mint = StateWithExtensionsMut::<SplToken2022Mint>::unpack_uninitialized(&mut mint_data).unwrap();
        let config = mint.init_extension::<TransferFeeConfig>(true).unwrap();
        let fee = TransferFee {
            epoch: 0_u64.into(),
            maximum_fee: 1_000_u64.into(),
            transfer_fee_basis_points: 1_000_u16.into(),
        };
        config.older_transfer_fee = fee;
        config.newer_transfer_fee = fee;
        mint.base = SplToken2022Mint {
            mint_authority: COption::None,
            supply: 0,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        };
        mint.pack_base();
        mint.init_account_type().unwrap();
    }
    let mint_key = Pubkey::new_unique();
    let token_program = Token2022::id();
    let mut lamports = 0;
    let mint = AccountInfo::new(
        &mint_key,
        false,
        false,
        &mut lamports,
        &mut mint_data,
        &token_program,
        false,
        0,
    );

    let (liquidator_debit, owner_debit) = liquidation_payout_debits(&mint, 1_000, 100, 0).unwrap();
    assert_eq!(liquidator_debit + owner_debit, 1_000);
    assert_eq!(liquidator_debit - get_transfer_fee_for_epoch(&mint, liquidator_debit, 0).unwrap(), 100);
    assert!(owner_debit < 900);

    // A small residual caps the gross-up instead of making liquidation fail.
    let (liquidator_debit, owner_debit) = liquidation_payout_debits(&mint, 105, 100, 0).unwrap();
    assert_eq!((liquidator_debit, owner_debit), (105, 0));
    assert!(liquidator_debit - get_transfer_fee_for_epoch(&mint, liquidator_debit, 0).unwrap() < 100);
}

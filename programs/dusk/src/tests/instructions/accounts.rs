use super::*;
use anchor_lang::solana_program::{program_option::COption, program_pack::Pack};
use spl_token_2022::{
    extension::{
        default_account_state::DefaultAccountState, group_member_pointer::GroupMemberPointer,
        group_pointer::GroupPointer, interest_bearing_mint::InterestBearingConfig,
        mint_close_authority::MintCloseAuthority, non_transferable::NonTransferable, pausable::PausableConfig,
        permanent_delegate::PermanentDelegate, scaled_ui_amount::ScaledUiAmountConfig, transfer_fee::TransferFeeConfig,
        transfer_hook::TransferHook, BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut,
    },
    state::Mint as SplToken2022Mint,
};

use spl_token_group_interface::state::{TokenGroup, TokenGroupMember};

fn extended_asset_mint_data(extensions: &[ExtensionType], authority: Pubkey) -> Vec<u8> {
    let mint_len = ExtensionType::try_calculate_account_len::<SplToken2022Mint>(extensions).unwrap();
    let mut data = vec![0_u8; mint_len];
    let mut mint = StateWithExtensionsMut::<SplToken2022Mint>::unpack_uninitialized(&mut data).unwrap();
    macro_rules! init {
        ($type:ty) => {
            mint.init_extension::<$type>(true).unwrap()
        };
    }
    for extension in extensions {
        match extension {
            ExtensionType::GroupPointer => {
                init!(GroupPointer);
            }
            ExtensionType::TokenGroup => {
                init!(TokenGroup);
            }
            ExtensionType::GroupMemberPointer => {
                init!(GroupMemberPointer);
            }
            ExtensionType::TokenGroupMember => {
                init!(TokenGroupMember);
            }
            ExtensionType::InterestBearingConfig => {
                init!(InterestBearingConfig);
            }
            ExtensionType::ScaledUiAmount => {
                let config = init!(ScaledUiAmountConfig);
                config.multiplier = 2.0.into();
                config.new_multiplier = 2.0.into();
            }
            ExtensionType::TransferHook => {
                init!(TransferHook).program_id = Some(crate::ID).try_into().unwrap();
            }
            ExtensionType::TransferFeeConfig => {
                init!(TransferFeeConfig);
            }
            ExtensionType::PermanentDelegate => {
                init!(PermanentDelegate);
            }
            ExtensionType::Pausable => {
                init!(PausableConfig);
            }
            ExtensionType::DefaultAccountState => {
                init!(DefaultAccountState);
            }
            ExtensionType::MintCloseAuthority => {
                init!(MintCloseAuthority);
            }
            ExtensionType::NonTransferable => {
                init!(NonTransferable);
            }
            _ => panic!("unsupported test fixture extension: {extension:?}"),
        }
    }
    mint.base = SplToken2022Mint {
        mint_authority: COption::Some(authority),
        supply: 0,
        decimals: 6,
        is_initialized: true,
        freeze_authority: COption::None,
    };
    mint.pack_base();
    mint.init_account_type().unwrap();
    data
}

#[test]
fn group_and_ui_extensions_are_assets_but_not_lp_receipts() {
    let market = Pubkey::new_unique();
    for extension in [
        ExtensionType::GroupPointer,
        ExtensionType::TokenGroup,
        ExtensionType::GroupMemberPointer,
        ExtensionType::TokenGroupMember,
        ExtensionType::InterestBearingConfig,
        ExtensionType::ScaledUiAmount,
    ] {
        for with_fee in [false, true] {
            // Put the new TLV first: older parsers can miss hooks or fees after it.
            let mut extensions = vec![extension, ExtensionType::TransferHook];
            if with_fee {
                extensions.push(ExtensionType::TransferFeeConfig);
            }
            let mut data = extended_asset_mint_data(&extensions, market);
            let key = Pubkey::new_unique();
            let owner = spl_token_2022::ID;
            let mut lamports = 1;
            let info = AccountInfo::new(&key, false, false, &mut lamports, &mut data, &owner, false, 0);
            let mint = InterfaceAccount::<Mint>::try_from(&info).unwrap();
            require_supported_asset_mint(&mint).unwrap();
            // A zero-fee configuration is still excluded from fee-free paths.
            assert_eq!(crate::token::is_fee_free_mint(&mint).unwrap(), !with_fee);
            assert_eq!(
                validate_lp_mint(&mint, market, 6).unwrap_err(),
                error!(ErrorCode::InvalidLpMintKey)
            );
        }
    }
}

#[test]
fn asset_extension_support_keeps_transfer_control_restrictions() {
    for extension in [
        ExtensionType::PermanentDelegate,
        ExtensionType::Pausable,
        ExtensionType::DefaultAccountState,
        ExtensionType::MintCloseAuthority,
        ExtensionType::NonTransferable,
    ] {
        let mut data = extended_asset_mint_data(&[ExtensionType::ScaledUiAmount, extension], Pubkey::new_unique());
        let key = Pubkey::new_unique();
        let owner = spl_token_2022::ID;
        let mut lamports = 1;
        let info = AccountInfo::new(&key, false, false, &mut lamports, &mut data, &owner, false, 0);
        let mint = InterfaceAccount::<Mint>::try_from(&info).unwrap();
        assert_eq!(
            require_supported_asset_mint(&mint).unwrap_err(),
            error!(ErrorCode::InvalidTokenProgram)
        );
        assert!(!crate::token::is_fee_free_mint(&mint).unwrap());
    }
}

#[test]
fn transfer_fees_after_scaled_ui_tlv_are_not_overlooked() {
    let mut data = extended_asset_mint_data(
        &[ExtensionType::ScaledUiAmount, ExtensionType::TransferFeeConfig],
        Pubkey::new_unique(),
    );
    {
        let mut mint = StateWithExtensionsMut::<SplToken2022Mint>::unpack(&mut data).unwrap();
        let fee = mint.get_extension_mut::<TransferFeeConfig>().unwrap();
        fee.older_transfer_fee.transfer_fee_basis_points = 100.into();
        fee.older_transfer_fee.maximum_fee = 1_000.into();
        fee.newer_transfer_fee = fee.older_transfer_fee;
    }
    let key = Pubkey::new_unique();
    let owner = spl_token_2022::ID;
    let mut lamports = 1;
    let info = AccountInfo::new(&key, false, false, &mut lamports, &mut data, &owner, false, 0);
    assert_eq!(crate::token::get_transfer_fee_for_epoch(&info, 10_000, 0).unwrap(), 100);
    assert_eq!(
        crate::token::get_transfer_inverse_fee_for_epoch(&info, 9_900, 0).unwrap(),
        100
    );
}

#[test]
fn live_market_assets_accept_high_decimal_mints() {
    for decimals in [0, 9, 10, 12, 18, 255] {
        let mint_key = Pubkey::new_unique();
        let mint_owner = spl_token::ID;
        let mut lamports = 1;
        let mut mint_data = vec![0_u8; SplToken2022Mint::LEN];
        SplToken2022Mint {
            mint_authority: COption::Some(Pubkey::new_unique()),
            supply: 0,
            decimals,
            is_initialized: true,
            freeze_authority: COption::None,
        }
        .pack_into_slice(&mut mint_data);
        let mint_info = AccountInfo::new(
            &mint_key,
            false,
            false,
            &mut lamports,
            &mut mint_data,
            &mint_owner,
            false,
            0,
        );
        let mint = InterfaceAccount::<Mint>::try_from(&mint_info).unwrap();
        require_supported_asset_mint(&mint).unwrap();
    }
}

#[test]
fn lp_mint_requires_an_immutable_dusk_transfer_hook() {
    let market = Pubkey::new_unique();
    for (hook_authority, accepted) in [(Some(Pubkey::new_unique()), false), (None, true)] {
        let mint_len =
            ExtensionType::try_calculate_account_len::<SplToken2022Mint>(&[ExtensionType::TransferHook]).unwrap();
        let mut mint_data = vec![0_u8; mint_len];
        {
            let mut mint = StateWithExtensionsMut::<SplToken2022Mint>::unpack_uninitialized(&mut mint_data).unwrap();
            let hook = mint.init_extension::<TransferHook>(true).unwrap();
            hook.authority = hook_authority.try_into().unwrap();
            hook.program_id = Some(crate::ID).try_into().unwrap();
            mint.base = SplToken2022Mint {
                mint_authority: COption::Some(market),
                supply: 0,
                decimals: 6,
                is_initialized: true,
                freeze_authority: COption::None,
            };
            mint.pack_base();
            mint.init_account_type().unwrap();
        }

        let mint_key = Pubkey::new_unique();
        let owner = spl_token_2022::ID;
        let mut lamports = 1;
        let mint_info = AccountInfo::new(&mint_key, false, false, &mut lamports, &mut mint_data, &owner, false, 0);
        let mint = InterfaceAccount::<Mint>::try_from(&mint_info).unwrap();
        let result = validate_lp_mint(&mint, market, 6);
        if accepted {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err(), error!(ErrorCode::InvalidLpMintKey));
        }
    }
}

#[test]
fn reserve_custody_covers_cash_fees_and_hlp_backing_without_rejecting_donations() {
    let mut side = MarketSide::default();
    side.reserves.cash_reserve = 100;
    side.reserves.base_hlp_backing_inventory = 7;
    side.reserves.quote_hlp_backing_inventory = 3;
    side.fees.swap_fee_custody_balance = 20;

    require_reserve_custody(130, &side).unwrap();
    require_reserve_custody(131, &side).unwrap();
    assert_eq!(
        require_reserve_custody(129, &side).unwrap_err(),
        error!(ErrorCode::UnbackedFeeLiability)
    );

    side.reserves.cash_reserve = u64::MAX;
    assert_eq!(
        require_reserve_custody(u64::MAX, &side).unwrap_err(),
        error!(ErrorCode::MarketMathOverflow)
    );
}

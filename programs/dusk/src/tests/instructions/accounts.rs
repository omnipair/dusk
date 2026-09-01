use super::*;
use anchor_lang::solana_program::{program_option::COption, program_pack::Pack};
use anchor_spl::token_interface::spl_token_2022::{
    extension::{transfer_hook::TransferHook, BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut},
    state::Mint as SplToken2022Mint,
};

#[test]
fn live_market_asset_decimals_are_bounded_by_nad_precision() {
    for (decimals, accepted) in [(0, true), (NAD_DECIMALS, true), (NAD_DECIMALS + 1, false)] {
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
        let result = require_supported_asset_mint(&mint);
        if accepted {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err(), error!(ErrorCode::UnsupportedAssetDecimals));
        }
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

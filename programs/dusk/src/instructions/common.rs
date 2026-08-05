use anchor_lang::{prelude::*, solana_program::program_option::COption};
use anchor_spl::{
    associated_token::get_associated_token_address_with_program_id,
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::{HLP_YLP_VAULT_SEED_PREFIX, NAD_DECIMALS},
    errors::ErrorCode,
    shared::token::{is_fee_free_mint, is_supported_mint, is_token_2022_mint, transfer_hook_config},
    state::{Market, MarketAsset, MarketSide},
};

pub fn derive_hlp_ylp_vault_address(market: Pubkey, target_hlp_mint: Pubkey, ylp_mint: Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            HLP_YLP_VAULT_SEED_PREFIX,
            market.as_ref(),
            target_hlp_mint.as_ref(),
            ylp_mint.as_ref(),
        ],
        &crate::ID,
    )
}

pub fn require_supported_asset_decimals(decimals: u8) -> Result<()> {
    require!(decimals <= NAD_DECIMALS, ErrorCode::UnsupportedAssetDecimals);
    Ok(())
}

macro_rules! market_update_and_validate {
    ($args:ty) => {
        pub fn update_and_validate(&mut self, args: &$args) -> Result<()> {
            self.market.update()?;
            self.validate(args)
        }
    };
    () => {
        pub fn update_and_validate(&mut self) -> Result<()> {
            self.market.update()?;
            self.validate()
        }
    };
}
pub(crate) use market_update_and_validate;

pub(crate) const HLP_SWAP_ACCOUNT_PREFIX_LEN: usize = 5;
pub(crate) const HLP_YLP_MINT_INDEX: usize = 0;
pub(crate) const BASE_HLP_YLP_VAULT_INDEX: usize = 1;
pub(crate) const QUOTE_HLP_YLP_VAULT_INDEX: usize = 2;
pub(crate) const BASE_INTEREST_VAULT_INDEX: usize = 3;
pub(crate) const QUOTE_INTEREST_VAULT_INDEX: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HlpSwapAccountLayout {
    pub(crate) prefix_len: usize,
}

impl<'info> TryFrom<(&Market, &[AccountInfo<'info>])> for HlpSwapAccountLayout {
    type Error = anchor_lang::error::Error;

    fn try_from((market, accounts): (&Market, &[AccountInfo<'info>])) -> Result<Self> {
        if !market.has_active_hlp() {
            return Ok(Self { prefix_len: 0 });
        }
        require_gte!(
            accounts.len(),
            HLP_SWAP_ACCOUNT_PREFIX_LEN,
            ErrorCode::NotEnoughAccounts
        );
        let expected = [
            (market.ylp_mint, ErrorCode::InvalidMint),
            (market.base_hlp_vault.ylp_vault, ErrorCode::InvalidVault),
            (market.quote_hlp_vault.ylp_vault, ErrorCode::InvalidVault),
            (market.base_side.interest_vault, ErrorCode::InvalidVault),
            (market.quote_side.interest_vault, ErrorCode::InvalidVault),
        ];
        for (account, (expected_key, error)) in accounts[..HLP_SWAP_ACCOUNT_PREFIX_LEN].iter().zip(expected) {
            require_keys_eq!(account.key(), expected_key, error);
            if !account.is_writable {
                return Err(error.into());
            }
        }
        require_keys_eq!(
            *accounts[HLP_YLP_MINT_INDEX].owner,
            Token2022::id(),
            ErrorCode::InvalidTokenProgram
        );
        if market.base_hlp_vault.hlp_supply > 0 || market.base_hlp_vault.residual_exposure != 0 {
            require_keys_eq!(
                *accounts[BASE_HLP_YLP_VAULT_INDEX].owner,
                Token2022::id(),
                ErrorCode::InvalidTokenProgram
            );
        }
        if market.quote_hlp_vault.hlp_supply > 0 || market.quote_hlp_vault.residual_exposure != 0 {
            require_keys_eq!(
                *accounts[QUOTE_HLP_YLP_VAULT_INDEX].owner,
                Token2022::id(),
                ErrorCode::InvalidTokenProgram
            );
        }
        for index in [BASE_INTEREST_VAULT_INDEX, QUOTE_INTEREST_VAULT_INDEX] {
            let owner = *accounts[index].owner;
            require!(
                owner == Token::id() || owner == Token2022::id(),
                ErrorCode::InvalidTokenProgram
            );
        }
        Ok(Self {
            prefix_len: HLP_SWAP_ACCOUNT_PREFIX_LEN,
        })
    }
}

impl HlpSwapAccountLayout {
    pub(crate) fn hook_accounts<'a, 'info>(&self, accounts: &'a [AccountInfo<'info>]) -> &'a [AccountInfo<'info>] {
        &accounts[self.prefix_len..]
    }
}

pub fn token_program_for_mint<'info>(
    mint: &InterfaceAccount<'info, Mint>,
    token_program: &Program<'info, Token>,
    token_2022_program: &Program<'info, Token2022>,
) -> Result<AccountInfo<'info>> {
    let mint_info = mint.to_account_info();
    if *mint_info.owner == token_program.key() {
        Ok(token_program.to_account_info())
    } else if *mint_info.owner == token_2022_program.key() {
        Ok(token_2022_program.to_account_info())
    } else {
        err!(ErrorCode::InvalidTokenProgram)
    }
}

pub fn require_supported_asset_mint(mint: &InterfaceAccount<Mint>) -> Result<()> {
    require!(is_supported_mint(mint)?, ErrorCode::InvalidTokenProgram);
    require_supported_asset_decimals(mint.decimals)?;
    Ok(())
}

pub fn validate_lp_mint(mint: &InterfaceAccount<Mint>, market: Pubkey, asset_decimals: u8) -> Result<()> {
    require!(is_token_2022_mint(mint)?, ErrorCode::InvalidLpMintKey);
    require!(is_fee_free_mint(mint)?, ErrorCode::InvalidLpMintKey);
    require!(
        transfer_hook_config(mint)? == Some((None, Some(crate::ID))),
        ErrorCode::InvalidLpMintKey
    );
    require_eq!(mint.decimals, asset_decimals, ErrorCode::WrongLpDecimals);
    require!(
        mint.mint_authority == COption::Some(market),
        ErrorCode::InvalidMintAuthority
    );
    require!(mint.freeze_authority == COption::None, ErrorCode::FrozenLpMint);
    Ok(())
}

pub fn token_account_credit(balance_before: u64, token_account: &InterfaceAccount<TokenAccount>) -> Result<u64> {
    token_account
        .amount
        .checked_sub(balance_before)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

pub fn token_account_debit(balance_before: u64, token_account: &InterfaceAccount<TokenAccount>) -> Result<u64> {
    balance_before
        .checked_sub(token_account.amount)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

/// Proves that the live reserve vault still backs both executable AMM cash
/// and every swap-fee atom deliberately excluded from that cash. Callers must
/// pass a balance read after their final reserve-vault token movement (or a
/// deterministically projected post-CPI balance on the optimized swap path).
pub(crate) fn require_reserve_custody(vault_balance: u64, market_side: &MarketSide) -> Result<()> {
    let required = market_side
        .reserves
        .cash_reserve
        .checked_add(market_side.fees.swap_fee_custody_balance)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    require_gte!(vault_balance, required, ErrorCode::UnbackedFeeLiability);
    Ok(())
}

/// Reads the live amount from a remaining-account token vault.
///
/// Remaining accounts are not wrapped in Anchor's cached `InterfaceAccount`,
/// so callers must deserialize the account again after a CPI to observe the
/// destination's actual Token-2022 credit.
pub fn token_account_info_amount(token_account: &AccountInfo) -> Result<u64> {
    require!(
        *token_account.owner == Token::id() || *token_account.owner == Token2022::id(),
        ErrorCode::InvalidTokenProgram
    );
    let data = token_account.try_borrow_data()?;
    let mut data_slice: &[u8] = &data;
    Ok(TokenAccount::try_deserialize_unchecked(&mut data_slice)?.amount)
}

pub fn token_account_info_credit(balance_before: u64, token_account: &AccountInfo) -> Result<u64> {
    token_account_info_amount(token_account)?
        .checked_sub(balance_before)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

#[cfg(test)]
mod decimal_tests {
    use super::*;
    use anchor_spl::token_interface::spl_token_2022::{
        extension::{transfer_hook::TransferHook, BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut},
        state::Mint as SplToken2022Mint,
    };

    #[test]
    fn live_market_asset_decimals_are_bounded_by_nad_precision() {
        for decimals in 0..=NAD_DECIMALS {
            require_supported_asset_decimals(decimals).unwrap();
        }

        let error = require_supported_asset_decimals(NAD_DECIMALS + 1).unwrap_err();
        match error {
            anchor_lang::error::Error::AnchorError(error) => {
                assert_eq!(error.error_name, "UnsupportedAssetDecimals");
            }
            other => panic!("unexpected error: {other:?}"),
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
                let mut mint =
                    StateWithExtensionsMut::<SplToken2022Mint>::unpack_uninitialized(&mut mint_data).unwrap();
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
    fn reserve_custody_covers_cash_and_excluded_swap_fees() {
        let mut side = MarketSide::default();
        side.reserves.cash_reserve = 100;
        side.fees.swap_fee_custody_balance = 20;

        require_reserve_custody(120, &side).unwrap();
        assert_eq!(
            require_reserve_custody(119, &side).unwrap_err(),
            error!(ErrorCode::UnbackedFeeLiability)
        );

        side.reserves.cash_reserve = u64::MAX;
        assert_eq!(
            require_reserve_custody(u64::MAX, &side).unwrap_err(),
            error!(ErrorCode::MarketMathOverflow)
        );
    }
}

pub fn validate_side_vault_accounts<'info>(
    market: &Account<'info, Market>,
    market_asset: MarketAsset,
    asset_mint: &InterfaceAccount<'info, Mint>,
    reserve_vault: &InterfaceAccount<'info, TokenAccount>,
) -> Result<()> {
    let market_side = market.side(market_asset);
    require_keys_eq!(market_side.asset_mint, asset_mint.key(), ErrorCode::InvalidMint);
    require_keys_eq!(market_side.reserve_vault, reserve_vault.key(), ErrorCode::InvalidVault);
    require_keys_eq!(reserve_vault.mint, asset_mint.key(), ErrorCode::InvalidVault);
    require_keys_eq!(reserve_vault.owner, market.key(), ErrorCode::InvalidVault);
    Ok(())
}

pub fn validate_owner_asset_account(
    owner: Pubkey,
    asset_mint: &InterfaceAccount<Mint>,
    owner_asset_account: &InterfaceAccount<TokenAccount>,
) -> Result<()> {
    require_keys_eq!(
        owner_asset_account.mint,
        asset_mint.key(),
        ErrorCode::InvalidTokenAccount
    );
    require_keys_eq!(owner_asset_account.owner, owner, ErrorCode::InvalidTokenAccount);
    Ok(())
}

pub fn validate_owner_lp_account(
    owner: Pubkey,
    lp_mint: &InterfaceAccount<Mint>,
    owner_lp_account: &InterfaceAccount<TokenAccount>,
) -> Result<()> {
    require_keys_eq!(owner_lp_account.mint, lp_mint.key(), ErrorCode::InvalidTokenAccount);
    require_keys_eq!(owner_lp_account.owner, owner, ErrorCode::InvalidTokenAccount);
    require_keys_eq!(
        *owner_lp_account.to_account_info().owner,
        Token2022::id(),
        ErrorCode::InvalidTokenAccount
    );
    validate_canonical_lp_token_account_key(owner_lp_account.key(), owner, lp_mint.key())?;
    Ok(())
}

/// Owner-scoped yield accounting has one checkpoint per owner, so every
/// externally held LP balance must live in that owner's one canonical ATA.
/// Internal protocol vaults are validated separately and intentionally do not
/// use this helper.
pub fn validate_canonical_lp_token_account_key(account: Pubkey, owner: Pubkey, lp_mint: Pubkey) -> Result<()> {
    let expected = get_associated_token_address_with_program_id(&owner, &lp_mint, &Token2022::id());
    require_keys_eq!(account, expected, ErrorCode::InvalidTokenAccount);
    Ok(())
}

pub fn validate_swap_fee_custody_accounts<'info>(
    market: &Account<'info, Market>,
    asset_mint: &InterfaceAccount<'info, Mint>,
    reserve_vault: &InterfaceAccount<'info, TokenAccount>,
) -> Result<MarketAsset> {
    let market_asset = market.asset_for_mint(asset_mint.key())?;
    let market_side = market.side(market_asset);
    require_keys_eq!(market_side.reserve_vault, reserve_vault.key(), ErrorCode::InvalidVault);
    require_keys_eq!(reserve_vault.mint, asset_mint.key(), ErrorCode::InvalidVault);
    require_keys_eq!(reserve_vault.owner, market.key(), ErrorCode::InvalidVault);
    Ok(market_asset)
}

pub fn validate_interest_accounts<'info>(
    market: &Account<'info, Market>,
    asset_mint: &InterfaceAccount<'info, Mint>,
    interest_vault: &InterfaceAccount<'info, TokenAccount>,
) -> Result<MarketAsset> {
    let market_asset = market.asset_for_mint(asset_mint.key())?;
    let market_side = market.side(market_asset);
    require_keys_eq!(
        market_side.interest_vault,
        interest_vault.key(),
        ErrorCode::InvalidVault
    );
    require_keys_eq!(interest_vault.mint, asset_mint.key(), ErrorCode::InvalidVault);
    require_keys_eq!(interest_vault.owner, market.key(), ErrorCode::InvalidVault);
    Ok(market_asset)
}

pub fn validate_swap_accounts<'info>(
    market: &Account<'info, Market>,
    trader: Pubkey,
    asset_in_mint: &InterfaceAccount<'info, Mint>,
    asset_out_mint: &InterfaceAccount<'info, Mint>,
    reserve_in_vault: &InterfaceAccount<'info, TokenAccount>,
    reserve_out_vault: &InterfaceAccount<'info, TokenAccount>,
    trader_asset_in_account: &InterfaceAccount<'info, TokenAccount>,
    trader_asset_out_account: &InterfaceAccount<'info, TokenAccount>,
) -> Result<MarketAsset> {
    let asset_in = market.asset_for_mint(asset_in_mint.key())?;
    let asset_out = market.asset_for_mint(asset_out_mint.key())?;
    require!(asset_out == asset_in.opposite(), ErrorCode::InvalidMint);

    let (market_side_in, market_side_out) = market.swap_sides(asset_in);
    require_keys_eq!(
        market_side_in.reserve_vault,
        reserve_in_vault.key(),
        ErrorCode::InvalidVault
    );
    require_keys_eq!(
        market_side_out.reserve_vault,
        reserve_out_vault.key(),
        ErrorCode::InvalidVault
    );
    require_keys_eq!(reserve_in_vault.mint, asset_in_mint.key(), ErrorCode::InvalidVault);
    require_keys_eq!(reserve_out_vault.mint, asset_out_mint.key(), ErrorCode::InvalidVault);
    require_keys_eq!(reserve_in_vault.owner, market.key(), ErrorCode::InvalidVault);
    require_keys_eq!(reserve_out_vault.owner, market.key(), ErrorCode::InvalidVault);

    validate_owner_asset_account(trader, asset_in_mint, trader_asset_in_account)?;
    validate_owner_asset_account(trader, asset_out_mint, trader_asset_out_account)?;
    Ok(asset_in)
}

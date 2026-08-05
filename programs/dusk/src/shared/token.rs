/// forked from raydium-cp-swap
/// https://github.com/raydium-io/raydium-cp-swap/blob/master/programs/cp-swap/src/utils/token.rs
/// Handles token transfers and minting with support for old token program and spl_token_2022
use crate::errors::ErrorCode;
use anchor_lang::{
    prelude::*,
    solana_program::{
        instruction::{AccountMeta, Instruction},
        program::invoke_signed,
    },
    system_program,
};
use anchor_spl::{
    token::{Token, TokenAccount},
    token_2022::{
        self,
        spl_token_2022::{
            self,
            extension::{
                transfer_fee::{TransferFeeConfig, MAX_FEE_BASIS_POINTS},
                transfer_hook, ExtensionType, StateWithExtensions,
            },
        },
        Token2022,
    },
    token_interface::{
        initialize_account3, spl_token_2022::extension::BaseStateWithExtensions, InitializeAccount3, Mint,
    },
};

#[allow(clippy::too_many_arguments)]
pub fn transfer_checked_with_remaining_accounts<'a>(
    authority: AccountInfo<'a>,
    from: AccountInfo<'a>,
    to: AccountInfo<'a>,
    mint: AccountInfo<'a>,
    token_program: AccountInfo<'a>,
    amount: u64,
    mint_decimals: u8,
    signer_seeds: &[&[&[u8]]],
    additional_accounts: &[AccountInfo<'a>],
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    if *token_program.key == Token2022::id() {
        let mut instruction = spl_token_2022::instruction::transfer_checked(
            token_program.key,
            from.key,
            mint.key,
            to.key,
            authority.key,
            &[],
            amount,
            mint_decimals,
        )?;
        let mut account_infos = vec![from.clone(), mint.clone(), to.clone(), authority.clone()];
        let transfer_hook_program_id = if *mint.owner != token_2022::Token2022::id() {
            None
        } else {
            let mint_data = mint.try_borrow_data()?;
            let mint_state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
            transfer_hook::get_program_id(&mint_state)
        };
        if let Some(transfer_hook_program_id) = transfer_hook_program_id {
            spl_transfer_hook_interface::onchain::add_extra_accounts_for_execute_cpi(
                &mut instruction,
                &mut account_infos,
                &transfer_hook_program_id,
                from,
                mint,
                to,
                authority,
                amount,
                additional_accounts,
            )?;
        }
        account_infos.push(token_program);
        invoke_signed(&instruction, &account_infos, signer_seeds).map_err(Into::into)
    } else if *token_program.key == Token::id() {
        let instruction = spl_token::instruction::transfer_checked(
            token_program.key,
            from.key,
            mint.key,
            to.key,
            authority.key,
            &[],
            amount,
            mint_decimals,
        )?;
        invoke_signed(&instruction, &[from, mint, to, authority, token_program], signer_seeds).map_err(Into::into)
    } else {
        err!(ErrorCode::InvalidTokenProgram)
    }
}

/// Issue a token `MintTo` instruction.
pub fn token_mint_to<'a>(
    authority: AccountInfo<'a>,
    token_program: AccountInfo<'a>,
    mint: AccountInfo<'a>,
    destination: AccountInfo<'a>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    if *token_program.key == Token2022::id() {
        invoke_signed(
            &spl_token_2022::instruction::mint_to(
                token_program.key,
                mint.key,
                destination.key,
                authority.key,
                &[],
                amount,
            )?,
            &[mint, destination, authority, token_program],
            signer_seeds,
        )
        .map_err(Into::into)
    } else if *token_program.key == Token::id() {
        invoke_signed(
            &spl_token::instruction::mint_to(token_program.key, mint.key, destination.key, authority.key, &[], amount)?,
            &[mint, destination, authority, token_program],
            signer_seeds,
        )
        .map_err(Into::into)
    } else {
        err!(ErrorCode::InvalidTokenProgram)
    }
}

pub fn token_mint_to_with_instruction<'a>(
    scratch: &mut Instruction,
    authority: AccountInfo<'a>,
    token_program: AccountInfo<'a>,
    mint: AccountInfo<'a>,
    destination: AccountInfo<'a>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    require!(
        *token_program.key == Token2022::id() || *token_program.key == Token::id(),
        ErrorCode::InvalidTokenProgram
    );
    scratch.program_id = *token_program.key;
    scratch.accounts.clear();
    scratch.accounts.push(AccountMeta::new(*mint.key, false));
    scratch.accounts.push(AccountMeta::new(*destination.key, false));
    scratch.accounts.push(AccountMeta::new_readonly(*authority.key, true));
    scratch.data.clear();
    scratch.data.push(7);
    scratch.data.extend_from_slice(&amount.to_le_bytes());
    invoke_signed(scratch, &[mint, destination, authority, token_program], signer_seeds).map_err(Into::into)
}

pub fn token_burn<'a>(
    authority: AccountInfo<'a>,
    token_program: AccountInfo<'a>,
    mint: AccountInfo<'a>,
    from: AccountInfo<'a>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    if *token_program.key == Token2022::id() {
        invoke_signed(
            &spl_token_2022::instruction::burn(token_program.key, from.key, mint.key, authority.key, &[], amount)?,
            &[from, mint, authority, token_program],
            signer_seeds,
        )
        .map_err(Into::into)
    } else if *token_program.key == Token::id() {
        invoke_signed(
            &spl_token::instruction::burn(token_program.key, from.key, mint.key, authority.key, &[], amount)?,
            &[from, mint, authority, token_program],
            signer_seeds,
        )
        .map_err(Into::into)
    } else {
        err!(ErrorCode::InvalidTokenProgram)
    }
}

/// Calculate the fee for output amount
pub fn get_transfer_inverse_fee(mint_info: &AccountInfo, post_fee_amount: u64) -> Result<u64> {
    get_transfer_inverse_fee_for_epoch(mint_info, post_fee_amount, Clock::get()?.epoch)
}

/// Calculate the fee for an output amount at a caller-supplied epoch.
///
/// Instructions that already read `Clock` should use this form so every fee
/// decision in the operation shares one sysvar snapshot.
pub fn get_transfer_inverse_fee_for_epoch(mint_info: &AccountInfo, post_fee_amount: u64, epoch: u64) -> Result<u64> {
    if *mint_info.owner == Token::id() {
        return Ok(0);
    }
    if post_fee_amount == 0 {
        return err!(ErrorCode::AmountZero);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;

    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        let transfer_fee = transfer_fee_config.get_epoch_fee(epoch);
        if u16::from(transfer_fee.transfer_fee_basis_points) == MAX_FEE_BASIS_POINTS {
            u64::from(transfer_fee.maximum_fee)
        } else {
            transfer_fee_config
                .calculate_inverse_epoch_fee(epoch, post_fee_amount)
                .ok_or(ErrorCode::MarketMathOverflow)?
        }
    } else {
        0
    };
    Ok(fee)
}

/// Calculate the fee for input amount
pub fn get_transfer_fee(mint_info: &AccountInfo, pre_fee_amount: u64) -> Result<u64> {
    get_transfer_fee_for_epoch(mint_info, pre_fee_amount, Clock::get()?.epoch)
}

/// Calculate the fee for an input amount at a caller-supplied epoch.
pub fn get_transfer_fee_for_epoch(mint_info: &AccountInfo, pre_fee_amount: u64, epoch: u64) -> Result<u64> {
    if *mint_info.owner == Token::id() {
        return Ok(0);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;

    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        transfer_fee_config
            .calculate_epoch_fee(epoch, pre_fee_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?
    } else {
        0
    };
    Ok(fee)
}

pub fn is_fee_free_mint(mint_account: &InterfaceAccount<Mint>) -> Result<bool> {
    let mint_info = mint_account.to_account_info();
    if *mint_info.owner == Token::id() {
        return Ok(true);
    }

    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
    let extensions = mint.get_extension_types()?;
    for e in extensions {
        if e == ExtensionType::TransferFeeConfig {
            return Ok(false);
        }
        if e != ExtensionType::MetadataPointer && e != ExtensionType::TokenMetadata && e != ExtensionType::TransferHook
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn create_token_account<'a>(
    authority: &AccountInfo<'a>,
    payer: &AccountInfo<'a>,
    token_account: &AccountInfo<'a>,
    mint_account: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
    token_program: &AccountInfo<'a>,
    signer_seeds: &[&[u8]],
) -> Result<()> {
    if token_account.owner == token_program.key && !token_account.data_is_empty() {
        let account_data = token_account.try_borrow_data()?;
        let account = StateWithExtensions::<spl_token_2022::state::Account>::unpack(&account_data)?;
        require_keys_eq!(account.base.mint, mint_account.key(), ErrorCode::InvalidMint);
        require_keys_eq!(account.base.owner, authority.key(), ErrorCode::InvalidVault);
        return Ok(());
    }

    let space = {
        let mint_info = mint_account.to_account_info();
        if *mint_info.owner == token_2022::Token2022::id() {
            let mint_data = mint_info.try_borrow_data()?;
            let mint_state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
            let mint_extensions = mint_state.get_extension_types()?;
            let required_extensions = ExtensionType::get_required_init_account_extensions(&mint_extensions);
            ExtensionType::try_calculate_account_len::<spl_token_2022::state::Account>(&required_extensions)?
        } else {
            TokenAccount::LEN
        }
    };
    let rent = Rent::get()?;
    let current_lamports = token_account.lamports();
    if current_lamports == 0 {
        let lamports = rent.minimum_balance(space);
        let cpi_accounts = system_program::CreateAccount {
            from: payer.to_account_info(),
            to: token_account.to_account_info(),
        };
        let cpi_context = CpiContext::new(system_program.to_account_info(), cpi_accounts);
        system_program::create_account(
            cpi_context.with_signer(&[signer_seeds]),
            lamports,
            u64::try_from(space).map_err(|_| ErrorCode::MarketMathOverflow)?,
            token_program.key,
        )?;
    } else {
        let required_lamports = rent.minimum_balance(space).max(1).saturating_sub(current_lamports);
        if required_lamports > 0 {
            let cpi_accounts = system_program::Transfer {
                from: payer.to_account_info(),
                to: token_account.to_account_info(),
            };
            let cpi_context = CpiContext::new(system_program.to_account_info(), cpi_accounts);
            system_program::transfer(cpi_context, required_lamports)?;
        }
        let cpi_accounts = system_program::Allocate {
            account_to_allocate: token_account.to_account_info(),
        };
        let cpi_context = CpiContext::new(system_program.to_account_info(), cpi_accounts);
        system_program::allocate(
            cpi_context.with_signer(&[signer_seeds]),
            u64::try_from(space).map_err(|_| ErrorCode::MarketMathOverflow)?,
        )?;

        let cpi_accounts = system_program::Assign {
            account_to_assign: token_account.to_account_info(),
        };
        let cpi_context = CpiContext::new(system_program.to_account_info(), cpi_accounts);
        system_program::assign(cpi_context.with_signer(&[signer_seeds]), token_program.key)?;
    }
    initialize_account3(CpiContext::new(
        token_program.to_account_info(),
        InitializeAccount3 {
            account: token_account.to_account_info(),
            mint: mint_account.to_account_info(),
            authority: authority.to_account_info(),
        },
    ))
}

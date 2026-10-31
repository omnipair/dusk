//! Token CPI helpers forked from Raydium CP Swap:
//! https://github.com/raydium-io/raydium-cp-swap/blob/master/programs/cp-swap/src/utils/token.rs
//!
//! Supports the legacy SPL Token program and Token-2022.

use anchor_lang::{
    prelude::*,
    solana_program::{instruction::Instruction, program::invoke_signed},
    system_program,
};
use anchor_spl::{
    token::{Token, TokenAccount},
    token_2022::{self, Token2022},
    token_interface::{initialize_account3, InitializeAccount3, Mint},
};
use spl_token_2022::extension::{
    transfer_fee::{TransferFeeConfig, MAX_FEE_BASIS_POINTS},
    transfer_hook, BaseStateWithExtensions, ExtensionType, StateWithExtensions,
};

use crate::errors::ErrorCode;

mod hook_accounts;
pub use hook_accounts::add_transfer_hook_accounts;

/// Invokes a mutable instruction without cloning its account and data vectors.
/// Solana's standard `invoke_signed` converts `instruction.clone()` into the
/// stable syscall layout, which permanently consumes another bump-allocation
/// in large composite instructions. This performs the same borrow checks,
/// lends the existing vectors to the stable layout, then restores them.
#[allow(unexpected_cfgs)]
pub(crate) fn invoke_signed_reusing_instruction<'a>(
    instruction: &mut Instruction,
    account_infos: &[AccountInfo<'a>],
    signer_seeds: &[&[&[u8]]],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    for account_meta in &instruction.accounts {
        for account_info in account_infos {
            if account_meta.pubkey == *account_info.key {
                if account_meta.is_writable {
                    let _ = account_info.try_borrow_mut_lamports()?;
                    let _ = account_info.try_borrow_mut_data()?;
                } else {
                    let _ = account_info.try_borrow_lamports()?;
                    let _ = account_info.try_borrow_data()?;
                }
                break;
            }
        }
    }

    #[cfg(target_os = "solana")]
    {
        use anchor_lang::solana_program::stable_layout::stable_instruction::StableInstruction;

        let stable_instruction = StableInstruction::from(Instruction {
            program_id: instruction.program_id,
            accounts: core::mem::take(&mut instruction.accounts),
            data: core::mem::take(&mut instruction.data),
        });
        let result = unsafe {
            anchor_lang::solana_program::syscalls::sol_invoke_signed_rust(
                &stable_instruction as *const _ as *const u8,
                account_infos as *const _ as *const u8,
                account_infos.len() as u64,
                signer_seeds as *const _ as *const u8,
                signer_seeds.len() as u64,
            )
        };
        let StableInstruction {
            accounts,
            data,
            program_id,
        } = stable_instruction;
        instruction.program_id = program_id;
        instruction.accounts = accounts.into();
        instruction.data = data.into();
        if result == anchor_lang::solana_program::entrypoint::SUCCESS {
            Ok(())
        } else {
            Err(result.into())
        }
    }

    #[cfg(not(target_os = "solana"))]
    anchor_lang::solana_program::program::invoke_signed_unchecked(instruction, account_infos, signer_seeds)
}

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
    let mut instruction = Instruction {
        program_id: Pubkey::default(),
        accounts: Vec::new(),
        data: Vec::new(),
    };
    transfer_checked_with_remaining_accounts_and_instruction(
        &mut instruction,
        authority,
        from,
        to,
        mint,
        token_program,
        amount,
        mint_decimals,
        signer_seeds,
        additional_accounts,
    )
}

/// Reuses one instruction buffer across repeated transfers in large composite
/// instructions. Solana's bump allocator does not reclaim the vectors created
/// by each SPL instruction, so rebuilding three or four payout transfers can
/// otherwise exhaust the heap even though every individual transfer is small.
#[allow(clippy::too_many_arguments)]
pub fn transfer_checked_with_remaining_accounts_and_instruction<'a>(
    instruction: &mut Instruction,
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
    require!(
        *token_program.key == Token2022::id() || *token_program.key == Token::id(),
        ErrorCode::InvalidTokenProgram
    );
    instruction.program_id = *token_program.key;
    instruction.accounts.clear();
    instruction.accounts.extend([
        AccountMeta::new(*from.key, false),
        AccountMeta::new_readonly(*mint.key, false),
        AccountMeta::new(*to.key, false),
        AccountMeta::new_readonly(*authority.key, true),
    ]);
    instruction.data.clear();
    // SPL Token and Token-2022 share the TransferChecked wire format.
    instruction.data.push(12);
    instruction.data.extend_from_slice(&amount.to_le_bytes());
    instruction.data.push(mint_decimals);
    if *token_program.key == Token2022::id() {
        let transfer_hook_program_id = if *mint.owner != token_2022::Token2022::id() {
            None
        } else {
            let mint_data = mint.try_borrow_data()?;
            let mint_state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
            transfer_hook::get_program_id(&mint_state)
        };
        if let Some(transfer_hook_program_id) = transfer_hook_program_id {
            let mut account_infos = vec![from.clone(), mint.clone(), to.clone(), authority.clone()];
            add_transfer_hook_accounts(
                instruction,
                &mut account_infos,
                &transfer_hook_program_id,
                from,
                mint,
                to,
                authority,
                amount,
                additional_accounts,
            )?;
            account_infos.push(token_program);
            invoke_signed_reusing_instruction(instruction, &account_infos, signer_seeds).map_err(Into::into)
        } else {
            invoke_signed_reusing_instruction(instruction, &[from, mint, to, authority, token_program], signer_seeds)
                .map_err(Into::into)
        }
    } else if *token_program.key == Token::id() {
        invoke_signed_reusing_instruction(instruction, &[from, mint, to, authority, token_program], signer_seeds)
            .map_err(Into::into)
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
#[inline(always)]
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
    Ok(mint.get_extension_types()?.into_iter().all(|extension| {
        SUPPORTED_ASSET_EXTENSIONS.contains(&extension) && extension != ExtensionType::TransferFeeConfig
    }))
}

pub(crate) const SUPPORTED_ASSET_EXTENSIONS: &[ExtensionType] = &[
    ExtensionType::TransferFeeConfig,
    ExtensionType::MetadataPointer,
    ExtensionType::TokenMetadata,
    ExtensionType::GroupPointer,
    ExtensionType::TokenGroup,
    ExtensionType::GroupMemberPointer,
    ExtensionType::TokenGroupMember,
    ExtensionType::InterestBearingConfig,
    ExtensionType::ScaledUiAmount,
];

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

#[cfg(test)]
mod hook_resolution_tests {
    use super::*;
    include!("tests/token_hook_resolution.rs");
}

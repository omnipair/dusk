use anchor_lang::{prelude::*, solana_program::program::invoke_signed};
use anchor_spl::{
    token,
    token_2022::{self, Token2022},
};
use spl_token_2022::extension::{
    transfer_fee::TransferFeeConfig, BaseStateWithExtensions, StateWithExtensions,
};

pub(crate) fn token_program_for_mint<'info>(
    mint: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    token_2022_program: &AccountInfo<'info>,
) -> AccountInfo<'info> {
    if mint.owner == token_program.key {
        token_program.clone()
    } else {
        token_2022_program.clone()
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn transfer_checked<'info>(
    token_program: AccountInfo<'info>,
    from: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
    additional_accounts: &[AccountInfo<'info>],
) -> Result<()> {
    dusk::token::transfer_checked_with_remaining_accounts(
        authority,
        from,
        to,
        mint,
        token_program,
        amount,
        decimals,
        signer_seeds,
        additional_accounts,
    )
}

pub(crate) fn close_token_account<'info>(
    token_program: AccountInfo<'info>,
    account: AccountInfo<'info>,
    destination: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if *token_program.key == Token2022::id() {
        token_2022::close_account(CpiContext::new_with_signer(
            token_program,
            token_2022::CloseAccount {
                account,
                destination,
                authority,
            },
            signer_seeds,
        ))
    } else {
        token::close_account(CpiContext::new_with_signer(
            token_program,
            token::CloseAccount {
                account,
                destination,
                authority,
            },
            signer_seeds,
        ))
    }
}

/// Transfer-fee mints leave withheld fees in the destination account even
/// after its spendable balance reaches zero. Harvest them before closing an
/// order vault; the mint's configured fee authority owns the harvested fees.
pub(crate) fn close_token_account_with_fee_harvest<'info>(
    token_program: AccountInfo<'info>,
    account: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    destination: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if *token_program.key == Token2022::id() {
        let has_transfer_fee = {
            let mint_data = mint.try_borrow_data()?;
            let mint_state =
                StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
            mint_state.get_extension::<TransferFeeConfig>().is_ok()
        };
        if has_transfer_fee {
            let instruction = spl_token_2022::extension::transfer_fee::instruction::harvest_withheld_tokens_to_mint(
                token_program.key,
                mint.key,
                &[account.key],
            )?;
            invoke_signed(
                &instruction,
                &[mint.clone(), account.clone(), token_program.clone()],
                &[],
            )?;
        }
    }
    close_token_account(token_program, account, destination, authority, signer_seeds)
}

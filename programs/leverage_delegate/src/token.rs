use anchor_lang::prelude::*;
use anchor_spl::{
    token,
    token_2022::{self, Token2022},
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

pub(crate) fn transfer_checked_with_signer<'info>(
    token_program: AccountInfo<'info>,
    from: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if *token_program.key == Token2022::id() {
        token_2022::transfer_checked(
            CpiContext::new_with_signer(
                token_program,
                token_2022::TransferChecked {
                    from,
                    mint,
                    to,
                    authority,
                },
                signer_seeds,
            ),
            amount,
            decimals,
        )
    } else {
        token::transfer_checked(
            CpiContext::new_with_signer(
                token_program,
                token::TransferChecked {
                    from,
                    mint,
                    to,
                    authority,
                },
                signer_seeds,
            ),
            amount,
            decimals,
        )
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
    remaining_accounts: &[AccountInfo<'info>],
) -> Result<()> {
    if *token_program.key == Token2022::id() {
        token_2022::transfer_checked(
            CpiContext::new_with_signer(
                token_program,
                token_2022::TransferChecked {
                    from,
                    mint,
                    to,
                    authority,
                },
                signer_seeds,
            )
            .with_remaining_accounts(remaining_accounts.to_vec()),
            amount,
            decimals,
        )
    } else {
        token::transfer_checked(
            CpiContext::new_with_signer(
                token_program,
                token::TransferChecked {
                    from,
                    mint,
                    to,
                    authority,
                },
                signer_seeds,
            ),
            amount,
            decimals,
        )
    }
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

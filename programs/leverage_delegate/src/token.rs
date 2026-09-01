use anchor_lang::{prelude::*, solana_program::program::invoke_signed};
use anchor_spl::{
    token,
    token_2022::{
        self,
        spl_token_2022::{
            self,
            extension::{transfer_hook, StateWithExtensions},
        },
        Token2022,
    },
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
            decimals,
        )?;
        let mut account_infos = vec![from.clone(), mint.clone(), to.clone(), authority.clone()];
        let transfer_hook_program_id = if *mint.owner != Token2022::id() {
            None
        } else {
            let mint_data = mint.try_borrow_data()?;
            let mint_state =
                StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
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

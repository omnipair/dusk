use anchor_lang::{prelude::*, system_program, Discriminator};

use crate::{
    account::{existing_pda_needs_growth, get_size_with_discriminator},
    errors::ErrorCode,
    state::YieldAccount,
};

/// Bring a yield account created by an earlier build up to the current size.
///
/// `YieldAccount` gained `harvest_authority` -- an `Option<Pubkey>`, thirty
/// three bytes -- and every account created before that deploy is still the
/// old length. Anchor deserializes the whole struct, so those accounts cannot
/// be opened at all: harvest, `set_harvest_authority`, single-sided hLP
/// deposits and governance support each fail with `AccountDidNotDeserialize`
/// before their handler runs. The holder has no way out, because every
/// instruction that could fix it needs to open the account first.
///
/// `add_liquidity` and `open_leverage` do grow it, because they go through
/// `initialize_pda_account_if_needed` rather than Anchor's deserializer. That
/// leaves an absurd precondition -- deposit liquidity before you may harvest
/// -- so this does the same repair on its own.
///
/// Permissionless on purpose. It appends zeroed bytes to reach the size the
/// program already expects and pays the rent difference from the caller's own
/// pocket; there is no state it can corrupt and no one it can take from. A
/// keeper can repair every stale account in the deployment without holding
/// any of them.
#[derive(Accounts)]
pub struct GrowYieldAccount<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: Validated by hand. Anchor's own deserializer is what cannot
    /// read this account, so a typed account here would fail the very case
    /// this instruction exists to repair.
    #[account(mut)]
    pub yield_account: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

impl<'info> GrowYieldAccount<'info> {
    pub fn handle_grow(ctx: Context<Self>) -> Result<()> {
        let account = ctx.accounts.yield_account.to_account_info();

        // Ours, and a yield account. Nothing else is touchable: the
        // discriminator is written at creation and is not something a caller
        // can forge into an account this program owns.
        require_keys_eq!(*account.owner, crate::ID, ErrorCode::InvalidArgument);
        {
            let data = account.try_borrow_data()?;
            require_gte!(data.len(), 8, ErrorCode::InvalidArgument);
            require!(
                &data[..8] == YieldAccount::DISCRIMINATOR,
                ErrorCode::InvalidArgument
            );
        }

        let space = get_size_with_discriminator::<YieldAccount>();
        // Already current. Not an error -- a keeper sweeping every account
        // should not have to know which ones it has done.
        if !existing_pda_needs_growth(account.data_len(), space)? {
            return Ok(());
        }

        // Rent before the resize. An account left under the minimum for its
        // new size is collectable, and the realloc itself would not complain.
        let rent = Rent::get()?;
        let top_up = rent
            .minimum_balance(space)
            .saturating_sub(account.lamports());
        if top_up > 0 {
            system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    system_program::Transfer {
                        from: ctx.accounts.payer.to_account_info(),
                        to: account.clone(),
                    },
                ),
                top_up,
            )?;
        }
        // Zero-initialised, so the bytes Borsh appended decode as the
        // default of whatever was added -- `None`, for an `Option`.
        account.realloc(space, true)?;

        Ok(())
    }
}

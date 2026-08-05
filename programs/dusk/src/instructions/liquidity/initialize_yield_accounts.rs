use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::*,
    errors::ErrorCode,
    instructions::transfer_hook::current_yield_contexts,
    shared::account::get_size_with_discriminator,
    state::{Market, YieldAccount, YieldTokenKind},
};

use super::initialize_or_validate_yield_account;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeYieldAccountsArgs {
    pub owner: Pubkey,
    pub token_kind: YieldTokenKind,
}

#[derive(Accounts)]
#[instruction(args: InitializeYieldAccountsArgs)]
pub struct InitializeYieldAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        init_if_needed,
        payer = payer,
        space = get_size_with_discriminator::<YieldAccount>(),
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            args.owner.as_ref(),
            lp_mint.key().as_ref(),
            base_mint.key().as_ref(),
            &[args.token_kind.code()],
        ],
        bump
    )]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(
        init_if_needed,
        payer = payer,
        space = get_size_with_discriminator::<YieldAccount>(),
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            args.owner.as_ref(),
            lp_mint.key().as_ref(),
            quote_mint.key().as_ref(),
            &[args.token_kind.code()],
        ],
        bump
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub system_program: Program<'info, System>,
}

impl<'info> InitializeYieldAccounts<'info> {
    pub fn validate(&self, args: &InitializeYieldAccountsArgs) -> Result<()> {
        require_keys_neq!(args.owner, Pubkey::default(), ErrorCode::InvalidArgument);
        require_keys_eq!(
            self.base_mint.key(),
            self.market.base_side.asset_mint,
            ErrorCode::InvalidMint
        );
        require_keys_eq!(
            self.quote_mint.key(),
            self.market.quote_side.asset_mint,
            ErrorCode::InvalidMint
        );
        match args.token_kind {
            YieldTokenKind::Ylp => {
                require_keys_eq!(self.lp_mint.key(), self.market.ylp_mint, ErrorCode::InvalidLpMintKey)
            }
            YieldTokenKind::Hlp => {
                self.market.asset_for_hlp_mint(self.lp_mint.key())?;
            }
        }
        Ok(())
    }

    pub fn handle_initialize(ctx: Context<'_, '_, '_, 'info, Self>, args: InitializeYieldAccountsArgs) -> Result<()> {
        let InitializeYieldAccounts {
            market,
            lp_mint,
            base_mint,
            quote_mint,
            base_yield_account,
            quote_yield_account,
            ..
        } = ctx.accounts;
        let market_key = market.key();
        let lp_mint = lp_mint.key();
        let base_mint = base_mint.key();
        let quote_mint = quote_mint.key();

        // Initialize or validate both asset ledgers before snapshotting new accounts.
        let base_initialized = initialize_or_validate_yield_account(
            base_yield_account,
            args.owner,
            market_key,
            lp_mint,
            base_mint,
            args.token_kind,
            ctx.bumps.base_yield_account,
        )?;
        let quote_initialized = initialize_or_validate_yield_account(
            quote_yield_account,
            args.owner,
            market_key,
            lp_mint,
            quote_mint,
            args.token_kind,
            ctx.bumps.quote_yield_account,
        )?;

        if !base_initialized && !quote_initialized {
            return Ok(());
        }

        // New accounts start at current indices so they cannot claim historical yield.
        market.accrue_interest()?;
        let contexts = current_yield_contexts(market, lp_mint)?.ok_or(error!(ErrorCode::InvalidLpMintKey))?;
        let base_context = contexts.items[0].ok_or(error!(ErrorCode::InvalidYieldAccount))?;
        let quote_context = contexts.items[1].ok_or(error!(ErrorCode::InvalidYieldAccount))?;
        require!(
            base_context.token_kind == args.token_kind && quote_context.token_kind == args.token_kind,
            ErrorCode::InvalidYieldAccount
        );
        if base_initialized {
            base_yield_account.accrue(
                0,
                base_context.swap_fee_growth_index_q64,
                base_context.interest_growth_index_q64,
            )?;
        }
        if quote_initialized {
            quote_yield_account.accrue(
                0,
                quote_context.swap_fee_growth_index_q64,
                quote_context.interest_growth_index_q64,
            )?;
        }
        Ok(())
    }
}

use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    account::get_size_with_discriminator,
    constants::*,
    errors::ErrorCode,
    instructions::transfer_hook::current_yield_contexts,
    state::{HlpCohort, Market, MarketAsset, YieldAccount, YieldTokenKind},
};

use super::initialize_or_validate_yield_account;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeYieldAccountsArgs {
    pub owner: Pubkey,
    pub token_kind: YieldTokenKind,
}

#[event_cpi]
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
            YieldTokenKind::Hlp | YieldTokenKind::Junior => {
                // A retired mint is checked against its cohort PDA in the
                // handler, supplied only for that path as a remaining account.
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
            event_authority,
            ..
        } = ctx.accounts;
        let market_key = market.key();
        let lp_mint = lp_mint.key();
        let base_mint = base_mint.key();
        let quote_mint = quote_mint.key();
        let retired_cohort = if market.asset_for_hlp_mint(lp_mint).is_err() && args.token_kind == YieldTokenKind::Hlp {
            Some(HlpCohort::from_remaining(
                ctx.remaining_accounts.first().ok_or(ErrorCode::InvalidLpMintKey)?,
                market_key,
                lp_mint,
            )?)
        } else {
            None
        };

        let retired_junior = if args.token_kind == YieldTokenKind::Junior && lp_mint != market.junior_mint {
            Some(crate::state::JuniorCohort::from_remaining(
                ctx.remaining_accounts.first().ok_or(ErrorCode::InvalidLpMintKey)?,
                market_key,
                lp_mint,
            )?)
        } else {
            None
        };

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
        crate::instructions::accounting::accrue_market_interest(
            market,
            Clock::get()?.slot,
            event_authority.to_account_info(),
        )?;
        let (base_swap, base_interest, quote_swap, quote_interest) =
            if let Some(contexts) = current_yield_contexts(market, lp_mint)? {
                let base_context = contexts.items[0].ok_or_else(|| error!(ErrorCode::InvalidYieldAccount))?;
                let quote_context = contexts.items[1].ok_or_else(|| error!(ErrorCode::InvalidYieldAccount))?;
                require!(
                    base_context.token_kind == args.token_kind && quote_context.token_kind == args.token_kind,
                    ErrorCode::InvalidYieldAccount
                );
                (
                    base_context.swap_fee_growth_index_q64,
                    base_context.interest_growth_index_q64,
                    quote_context.swap_fee_growth_index_q64,
                    quote_context.interest_growth_index_q64,
                )
            } else if let Some(cohort) = &retired_junior {
                let (bs, bi) = cohort.yield_indexes(MarketAsset::Base);
                let (qs, qi) = cohort.yield_indexes(MarketAsset::Quote);
                (bs, bi, qs, qi)
            } else {
                let cohort = retired_cohort.as_ref().ok_or(ErrorCode::InvalidLpMintKey)?;
                let (base_swap, base_interest) = cohort.yield_indexes(MarketAsset::Base);
                let (quote_swap, quote_interest) = cohort.yield_indexes(MarketAsset::Quote);
                (base_swap, base_interest, quote_swap, quote_interest)
            };
        if base_initialized {
            market.checkpoint_holder_income(base_yield_account, 0, retired_junior.as_ref().map(|c| c.income_end))?;
            base_yield_account.accrue(0, base_swap, base_interest)?;
        }
        if quote_initialized {
            market.checkpoint_holder_income(quote_yield_account, 0, retired_junior.as_ref().map(|c| c.income_end))?;
            quote_yield_account.accrue(0, quote_swap, quote_interest)?;
        }
        Ok(())
    }
}

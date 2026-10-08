use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022};
use spl_token_2022::{extension::StateWithExtensions, state::Account as SplToken2022Account};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{MarketEventMetadata, YieldAccountsClosed},
    instructions::accounts::validate_canonical_lp_token_account_key,
    state::{Market, YieldAccount, YieldTokenKind},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseYieldAccountsArgs {
    pub token_kind: YieldTokenKind,
}

/// Close the owner's pair of yield accounts for one LP mint and return their
/// rent to the owner. Only an empty pair closes: the owner holds none of that
/// LP and nothing accrued is left to harvest, so closing forfeits at most the
/// sub-atom remainders. LP only reaches an owner through instructions that
/// need the pair (deposits, the transfer hook, governance withdrawals), and
/// `initialize_yield_accounts` recreates it at the current yield indexes.
#[event_cpi]
#[derive(Accounts)]
#[instruction(args: CloseYieldAccountsArgs)]
pub struct CloseYieldAccounts<'info> {
    #[account(
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,

    /// CHECK: The owner's canonical LP token account, the only account that
    /// can hold the owner's LP. It must hold none, or not exist.
    pub owner_lp_account: UncheckedAccount<'info>,

    #[account(
        mut,
        close = owner,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            lp_mint.key().as_ref(),
            market.base_side.asset_mint.as_ref(),
            &[args.token_kind.code()],
        ],
        bump = base_yield_account.bump
    )]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(
        mut,
        close = owner,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            lp_mint.key().as_ref(),
            market.quote_side.asset_mint.as_ref(),
            &[args.token_kind.code()],
        ],
        bump = quote_yield_account.bump
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,
}

impl<'info> CloseYieldAccounts<'info> {
    pub fn validate(&self, args: &CloseYieldAccountsArgs) -> Result<()> {
        match args.token_kind {
            YieldTokenKind::Ylp => {
                require_keys_eq!(self.lp_mint.key(), self.market.ylp_mint, ErrorCode::InvalidMint)
            }
            YieldTokenKind::Hlp => {
                self.market.asset_for_hlp_mint(self.lp_mint.key())?;
            }
        }
        for (yield_account, asset_mint) in [
            (&self.base_yield_account, self.market.base_side.asset_mint),
            (&self.quote_yield_account, self.market.quote_side.asset_mint),
        ] {
            yield_account.assert_account(
                self.owner.key(),
                self.market.key(),
                self.lp_mint.key(),
                asset_mint,
                args.token_kind,
            )?;
            require_eq!(yield_account.claimable_amount()?, 0, ErrorCode::YieldAccountsNotEmpty);
        }
        validate_canonical_lp_token_account_key(self.owner_lp_account.key(), self.owner.key(), self.lp_mint.key())?;
        if !self.owner_lp_account.data_is_empty() {
            require_keys_eq!(
                *self.owner_lp_account.owner,
                Token2022::id(),
                ErrorCode::InvalidTokenAccount
            );
            let data = self.owner_lp_account.try_borrow_data()?;
            let lp_account = StateWithExtensions::<SplToken2022Account>::unpack(&data)?;
            require_eq!(lp_account.base.amount, 0, ErrorCode::YieldAccountsNotEmpty);
        }
        Ok(())
    }

    pub fn handle_close(ctx: Context<Self>, args: CloseYieldAccountsArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        emit_cpi!(YieldAccountsClosed {
            market: market_key,
            owner: owner_key,
            lp_mint: ctx.accounts.lp_mint.key(),
            token_kind: args.token_kind.code(),
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}

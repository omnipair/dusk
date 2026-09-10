use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{HarvestAuthorityUpdated, MarketEventMetadata},
    state::{Market, YieldAccount, YieldTokenKind},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetHarvestAuthorityArgs {
    pub token_kind: YieldTokenKind,
    /// `None` revokes the independent caller; the recipient is unchanged.
    pub harvest_authority: Option<Pubkey>,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: SetHarvestAuthorityArgs)]
pub struct SetHarvestAuthority<'info> {
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

    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            lp_mint.key().as_ref(),
            asset_mint.key().as_ref(),
            &[args.token_kind.code()],
        ],
        bump = yield_account.bump
    )]
    pub yield_account: Box<Account<'info, YieldAccount>>,
}

impl<'info> SetHarvestAuthority<'info> {
    pub fn validate(&self, args: &SetHarvestAuthorityArgs) -> Result<()> {
        if let Some(authority) = args.harvest_authority {
            require_keys_neq!(authority, Pubkey::default(), ErrorCode::InvalidArgument);
        }
        self.market.asset_for_mint(self.asset_mint.key())?;
        match args.token_kind {
            YieldTokenKind::Ylp => {
                require_keys_eq!(self.lp_mint.key(), self.market.ylp_mint, ErrorCode::InvalidMint)
            }
            YieldTokenKind::Hlp => {
                self.market.asset_for_hlp_mint(self.lp_mint.key())?;
            }
        }
        self.yield_account.assert_account(
            self.owner.key(),
            self.market.key(),
            self.lp_mint.key(),
            self.asset_mint.key(),
            args.token_kind,
        )
    }

    pub fn handle_set(ctx: Context<Self>, args: SetHarvestAuthorityArgs) -> Result<()> {
        let SetHarvestAuthority {
            market,
            owner,
            asset_mint,
            lp_mint,
            yield_account,
            ..
        } = ctx.accounts;
        let market_key = market.key();
        let owner_key = owner.key();

        yield_account.harvest_authority = args.harvest_authority;
        emit_cpi!(HarvestAuthorityUpdated {
            market: market_key,
            owner: owner_key,
            lp_mint: lp_mint.key(),
            asset_mint: asset_mint.key(),
            token_kind: args.token_kind.code(),
            harvest_authority: args.harvest_authority,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}

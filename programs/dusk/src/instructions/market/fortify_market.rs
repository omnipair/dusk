use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    events::{InsuranceDonated, MarketEventMetadata},
    instructions::accounts::{
        require_supported_asset_mint, token_account_credit, token_program_for_mint, validate_owner_asset_account,
    },
    state::{Market, MarketAsset},
    token::transfer_checked_with_remaining_accounts,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FortifyMarketArgs {
    pub asset: u8,
    pub amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: FortifyMarketArgs)]
pub struct FortifyMarket<'info> {
    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut)]
    pub donor: Signer<'info>,

    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub donor_asset_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> FortifyMarket<'info> {
    pub fn validate(&self, args: &FortifyMarketArgs) -> Result<()> {
        self.market.assert_current_version()?;
        require!(args.amount > 0, ErrorCode::AmountZero);
        let asset = MarketAsset::try_from_code(args.asset)?;
        let side = self.market.side(asset);
        require_keys_eq!(side.asset_mint, self.asset_mint.key(), ErrorCode::InvalidMint);
        let expected_vault = match asset {
            MarketAsset::Base => self.market.insurance.base_vault,
            MarketAsset::Quote => self.market.insurance.quote_vault,
        };
        require_keys_eq!(expected_vault, self.insurance_vault.key(), ErrorCode::InvalidVault);
        require_keys_eq!(
            self.insurance_vault.mint,
            self.asset_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(self.insurance_vault.owner, self.market.key(), ErrorCode::InvalidVault);
        validate_owner_asset_account(self.donor.key(), &self.asset_mint, &self.donor_asset_account)?;
        require_supported_asset_mint(&self.asset_mint)
    }

    pub fn handle_fortify(ctx: Context<'_, '_, '_, 'info, Self>, args: FortifyMarketArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let donor_key = ctx.accounts.donor.key();
        let asset = MarketAsset::try_from_code(args.asset)?;
        let balance_before = ctx.accounts.insurance_vault.amount;
        let token_program = token_program_for_mint(
            &ctx.accounts.asset_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.donor.to_account_info(),
            ctx.accounts.donor_asset_account.to_account_info(),
            ctx.accounts.insurance_vault.to_account_info(),
            ctx.accounts.asset_mint.to_account_info(),
            token_program,
            args.amount,
            ctx.accounts.asset_mint.decimals,
            &[],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.insurance_vault.reload()?;
        let actual_credit = token_account_credit(balance_before, &ctx.accounts.insurance_vault)?;
        require!(actual_credit > 0, ErrorCode::AmountZero);
        ctx.accounts
            .market
            .credit_insurance_donation(asset, actual_credit, Clock::get()?.slot)?;

        emit_cpi!(InsuranceDonated {
            market: market_key,
            donor: donor_key,
            asset: asset.code(),
            requested_amount: args.amount,
            credited_amount: actual_credit,
            available_after: ctx.accounts.market.insurance.available(asset),
            metadata: MarketEventMetadata::new(donor_key, market_key)?,
        });
        Ok(())
    }
}

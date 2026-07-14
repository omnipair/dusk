use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::log::emit_hlp_opened_low_heap,
    generate_market_seeds,
    shared::{
        account::get_size_with_discriminator,
        token::{token_mint_to_with_scratch, transfer_from_user_to_vault, TokenInstructionScratch},
    },
    state::{FutarchyAuthority, Market, MarketAsset, YieldAccount, YieldTokenKind},
};

use crate::instructions::common::{
    require_supported_asset_mint, token_program_for_mint, validate_lp_mint, validate_owner_asset_account,
    validate_owner_lp_account, validate_side_vault_accounts,
};

use super::initialize_or_validate_hlp_yield_account;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct DepositSingleSidedArgs {
    pub deposit_amount: u64,
    pub min_hlp_amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: DepositSingleSidedArgs)]
pub struct DepositSingleSided<'info> {
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

    #[account(
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub target_hlp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner_target_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = owner,
        seeds = [
            HLP_YLP_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            target_hlp_mint.key().as_ref(),
            ylp_mint.key().as_ref(),
        ],
        bump,
        token::mint = ylp_mint,
        token::authority = market,
        token::token_program = token_2022_program,
    )]
    pub hlp_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        init_if_needed,
        payer = owner,
        space = get_size_with_discriminator::<YieldAccount>(),
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            owner_target_account.mint.as_ref(),
            &[YieldTokenKind::Hlp.code()],
        ],
        bump
    )]
    pub target_yield_account: Box<Account<'info, YieldAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> DepositSingleSided<'info> {
    pub fn validate(&self, args: &DepositSingleSidedArgs) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        require!(args.deposit_amount > 0, ErrorCode::AmountZero);
        validate_side_vault_accounts(
            &self.market,
            MarketAsset::Base,
            &self.base_mint,
            &self.base_reserve_vault,
        )?;
        validate_side_vault_accounts(
            &self.market,
            MarketAsset::Quote,
            &self.quote_mint,
            &self.quote_reserve_vault,
        )?;
        require_keys_eq!(self.market.ylp_mint, self.ylp_mint.key(), ErrorCode::InvalidLpMintKey);
        let target_asset = self.market.asset_for_hlp_mint(self.target_hlp_mint.key())?;
        let target_mint = match target_asset {
            MarketAsset::Base => &self.base_mint,
            MarketAsset::Quote => &self.quote_mint,
        };
        let target_hlp_mint = self.market.side(target_asset)?.hlp_mint;
        require_keys_eq!(target_hlp_mint, self.target_hlp_mint.key(), ErrorCode::InvalidMint);
        validate_owner_asset_account(self.owner.key(), target_mint, &self.owner_target_account)?;
        validate_owner_lp_account(self.owner.key(), &self.target_hlp_mint, &self.owner_hlp_account)?;
        validate_lp_mint(&self.target_hlp_mint, self.market.key(), target_mint.decimals)?;
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.base_mint.decimals)?;
        require_keys_eq!(
            self.hlp_ylp_account.mint,
            self.ylp_mint.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_eq!(self.hlp_ylp_account.owner, self.market.key(), ErrorCode::InvalidVault);
        require_supported_asset_mint(&self.base_mint)?;
        require_supported_asset_mint(&self.quote_mint)?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(DepositSingleSidedArgs);

    pub fn handle_deposit(ctx: Context<Self>, args: DepositSingleSidedArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let target_asset = ctx
            .accounts
            .market
            .asset_for_hlp_mint(ctx.accounts.target_hlp_mint.key())?;
        let target_mint_key = match target_asset {
            MarketAsset::Base => ctx.accounts.base_mint.key(),
            MarketAsset::Quote => ctx.accounts.quote_mint.key(),
        };

        ctx.accounts.market.refresh_risk()?;
        ctx.accounts.market.assert_risk_circuit_breakers()?;

        let (target_reserve_vault, target_mint) = match target_asset {
            MarketAsset::Base => (
                ctx.accounts.base_reserve_vault.to_account_info(),
                ctx.accounts.base_mint.to_account_info(),
            ),
            MarketAsset::Quote => (
                ctx.accounts.quote_reserve_vault.to_account_info(),
                ctx.accounts.quote_mint.to_account_info(),
            ),
        };
        let reserve_before = match target_asset {
            MarketAsset::Base => ctx.accounts.base_reserve_vault.amount,
            MarketAsset::Quote => ctx.accounts.quote_reserve_vault.amount,
        };
        let target_token_program = token_program_for_mint(
            match target_asset {
                MarketAsset::Base => &ctx.accounts.base_mint,
                MarketAsset::Quote => &ctx.accounts.quote_mint,
            },
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_from_user_to_vault(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.owner_target_account.to_account_info(),
            target_reserve_vault,
            target_mint,
            target_token_program,
            args.deposit_amount,
            match target_asset {
                MarketAsset::Base => ctx.accounts.base_mint.decimals,
                MarketAsset::Quote => ctx.accounts.quote_mint.decimals,
            },
        )?;
        match target_asset {
            MarketAsset::Base => ctx.accounts.base_reserve_vault.reload()?,
            MarketAsset::Quote => ctx.accounts.quote_reserve_vault.reload()?,
        }
        let deposit_credit = match target_asset {
            MarketAsset::Base => ctx.accounts.base_reserve_vault.amount.checked_sub(reserve_before),
            MarketAsset::Quote => ctx.accounts.quote_reserve_vault.amount.checked_sub(reserve_before),
        }
        .ok_or(ErrorCode::MarketMathOverflow)?;

        let receipt = ctx
            .accounts
            .market
            .deposit_single_sided(target_asset, deposit_credit, args.min_hlp_amount)?;
        initialize_or_validate_hlp_yield_account(
            &mut ctx.accounts.target_yield_account,
            owner_key,
            market_key,
            target_mint_key,
            ctx.bumps.target_yield_account,
        )?;
        let (swap_fee_growth_index_nad, interest_growth_index_nad) =
            ctx.accounts.market.hlp_yield_growth_indexes(target_asset);
        ctx.accounts.target_yield_account.accrue(
            ctx.accounts.owner_hlp_account.amount,
            swap_fee_growth_index_nad,
            interest_growth_index_nad,
        )?;

        let ylp_program = token_program_for_mint(
            &ctx.accounts.ylp_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let hlp_program = token_program_for_mint(
            &ctx.accounts.target_hlp_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let market_seeds = generate_market_seeds!(ctx.accounts.market);
        let signer_seeds = [&market_seeds[..]];
        let mut mint_scratch = TokenInstructionScratch::new(*ylp_program.key);
        token_mint_to_with_scratch(
            &mut mint_scratch,
            ctx.accounts.market.to_account_info(),
            ylp_program.clone(),
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.hlp_ylp_account.to_account_info(),
            receipt.ylp_amount,
            &signer_seeds,
        )?;
        token_mint_to_with_scratch(
            &mut mint_scratch,
            ctx.accounts.market.to_account_info(),
            hlp_program,
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.owner_hlp_account.to_account_info(),
            receipt.hlp_amount,
            &signer_seeds,
        )?;

        emit_hlp_opened_low_heap(
            market_key,
            owner_key,
            target_mint_key,
            receipt.deposit_amount,
            receipt.borrowed_amount,
            receipt.ylp_amount,
            receipt.hlp_amount,
            receipt.hlp_supply,
        )?;

        Ok(())
    }
}

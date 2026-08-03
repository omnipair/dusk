use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::{FUTARCHY_AUTHORITY_SEED_PREFIX, MARKET_V2_SEED_PREFIX},
    errors::ErrorCode,
    events::log::emit_hlp_rebalanced_low_heap,
    generate_market_seeds,
    instructions::common::{
        require_supported_asset_mint, token_account_credit, token_program_for_mint, validate_interest_accounts,
        validate_lp_mint, validate_side_vault_accounts,
    },
    shared::token::{
        token_burn, token_mint_to_with_scratch, transfer_from_vault_to_vault_with_remaining_accounts,
        TokenInstructionScratch,
    },
    state::{FutarchyAuthority, HlpRebalanceReceipt, Market, MarketAsset},
};

use super::record_hlp_interest_credit;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct CrankHlpRebalanceArgs {
    pub target_asset: u8,
}

/// Permissionless, bounded hLP inventory correction for exactly one target
/// vault. The opposite hLP vault is deliberately absent from this transition.
#[derive(Accounts)]
#[instruction(args: CrankHlpRebalanceArgs)]
pub struct CrankHlpRebalance<'info> {
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

    /// Anyone may settle already-recorded hLP exposure.
    pub keeper: Signer<'info>,

    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub base_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub hlp_ylp_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub borrowed_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CrankHlpRebalance<'info> {
    pub fn validate(&self, args: &CrankHlpRebalanceArgs) -> Result<()> {
        self.market.assert_live_with_futarchy(&self.futarchy_authority)?;
        let target_asset = MarketAsset::try_from_code(args.target_asset)?;
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
        require_supported_asset_mint(&self.base_mint)?;
        require_supported_asset_mint(&self.quote_mint)?;
        require_keys_eq!(self.market.ylp_mint, self.ylp_mint.key(), ErrorCode::InvalidLpMintKey);
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.base_mint.decimals)?;

        let target_vault = match target_asset {
            MarketAsset::Base => &self.market.base_hlp_vault,
            MarketAsset::Quote => &self.market.quote_hlp_vault,
        };
        require!(
            target_vault.hlp_supply > 0 || target_vault.pending_rebalance != 0,
            ErrorCode::HlpSettlementUnavailable
        );
        require_keys_eq!(
            target_vault.ylp_vault,
            self.hlp_ylp_account.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.hlp_ylp_account.mint,
            self.ylp_mint.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_eq!(self.hlp_ylp_account.owner, self.market.key(), ErrorCode::InvalidVault);
        require_gte!(
            self.hlp_ylp_account.amount,
            target_vault.ylp_shares,
            ErrorCode::InsufficientBalance
        );

        let borrowed_asset = target_asset.opposite();
        let borrowed_mint = match borrowed_asset {
            MarketAsset::Base => &self.base_mint,
            MarketAsset::Quote => &self.quote_mint,
        };
        let interest_asset = validate_interest_accounts(&self.market, borrowed_mint, &self.borrowed_interest_vault)?;
        require!(interest_asset == borrowed_asset, ErrorCode::InvalidVault);
        Ok(())
    }

    pub fn update(&mut self) -> Result<()> {
        self.market.accrue_interest()
    }

    pub fn update_and_validate(&mut self, args: &CrankHlpRebalanceArgs) -> Result<()> {
        self.update()?;
        self.validate(args)
    }

    pub fn handle_crank(mut ctx: Context<'_, '_, '_, 'info, Self>, args: CrankHlpRebalanceArgs) -> Result<()> {
        let current_slot = Clock::get()?.slot;
        let target_asset = MarketAsset::try_from_code(args.target_asset)?;
        let receipt = ctx.accounts.market.rebalance_hlp_vault(target_asset)?;
        require!(
            receipt.ylp_mint_amount == 0 || receipt.ylp_burn_amount == 0,
            ErrorCode::BrokenInvariant
        );

        apply_ylp_change(&mut ctx, &receipt)?;
        apply_interest_change(&mut ctx, &receipt)?;

        ctx.accounts
            .market
            .checkpoint_amm_neutral_inventory_and_observe_risk(current_slot)?;
        ctx.accounts.market.assert_market_health()?;

        emit_hlp_rebalanced_low_heap(
            ctx.accounts.market.key(),
            ctx.accounts.keeper.key(),
            target_asset.code(),
            receipt.ideal_delta,
            receipt.executed_delta,
            receipt.pending_rebalance,
            receipt.nav_nad,
            current_slot,
        );
        Ok(())
    }
}

fn apply_ylp_change<'info>(
    ctx: &mut Context<'_, '_, '_, 'info, CrankHlpRebalance<'info>>,
    receipt: &HlpRebalanceReceipt,
) -> Result<()> {
    if receipt.ylp_mint_amount == 0 && receipt.ylp_burn_amount == 0 {
        return Ok(());
    }
    let ylp_program = token_program_for_mint(
        &ctx.accounts.ylp_mint,
        &ctx.accounts.token_program,
        &ctx.accounts.token_2022_program,
    )?;
    let market_seeds = generate_market_seeds!(ctx.accounts.market);
    let signer_seeds = [&market_seeds[..]];

    if receipt.ylp_mint_amount > 0 {
        let mut scratch = TokenInstructionScratch::new(*ylp_program.key);
        token_mint_to_with_scratch(
            &mut scratch,
            ctx.accounts.market.to_account_info(),
            ylp_program,
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.hlp_ylp_account.to_account_info(),
            receipt.ylp_mint_amount,
            &signer_seeds,
        )?;
    } else {
        require_gte!(
            ctx.accounts.hlp_ylp_account.amount,
            receipt.ylp_burn_amount,
            ErrorCode::InsufficientBalance
        );
        token_burn(
            ctx.accounts.market.to_account_info(),
            ylp_program,
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.hlp_ylp_account.to_account_info(),
            receipt.ylp_burn_amount,
            &signer_seeds,
        )?;
    }
    Ok(())
}

fn apply_interest_change<'info>(
    ctx: &mut Context<'_, '_, '_, 'info, CrankHlpRebalance<'info>>,
    receipt: &HlpRebalanceReceipt,
) -> Result<()> {
    if receipt.interest_paid == 0 {
        return Ok(());
    }
    let borrowed_asset = receipt.target_asset.opposite();
    let (reserve_vault, mint, decimals) = match borrowed_asset {
        MarketAsset::Base => (
            ctx.accounts.base_reserve_vault.to_account_info(),
            ctx.accounts.base_mint.to_account_info(),
            ctx.accounts.base_mint.decimals,
        ),
        MarketAsset::Quote => (
            ctx.accounts.quote_reserve_vault.to_account_info(),
            ctx.accounts.quote_mint.to_account_info(),
            ctx.accounts.quote_mint.decimals,
        ),
    };
    let borrowed_token_program = token_program_for_mint(
        match borrowed_asset {
            MarketAsset::Base => &ctx.accounts.base_mint,
            MarketAsset::Quote => &ctx.accounts.quote_mint,
        },
        &ctx.accounts.token_program,
        &ctx.accounts.token_2022_program,
    )?;
    let interest_vault_balance_before = ctx.accounts.borrowed_interest_vault.amount;
    transfer_from_vault_to_vault_with_remaining_accounts(
        ctx.accounts.market.to_account_info(),
        reserve_vault,
        ctx.accounts.borrowed_interest_vault.to_account_info(),
        mint,
        borrowed_token_program,
        receipt.interest_paid,
        decimals,
        &[&generate_market_seeds!(ctx.accounts.market)[..]],
        ctx.remaining_accounts,
    )?;
    ctx.accounts.borrowed_interest_vault.reload()?;
    let interest_vault_credit =
        token_account_credit(interest_vault_balance_before, &ctx.accounts.borrowed_interest_vault)?;
    let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
    record_hlp_interest_credit(
        ctx.accounts.market.side_mut(borrowed_asset),
        interest_vault_credit,
        manager_fee_bps,
        ctx.accounts.futarchy_authority.revenue_share.interest_bps,
        ctx.accounts.futarchy_authority.protocol_auction_split,
    )
}

use anchor_lang::prelude::*;
use anchor_lang::solana_program::log::sol_log_data;
use anchor_lang::Discriminator;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::HlpClosed,
    generate_market_seeds,
    shared::token::{token_burn, transfer_checked_with_remaining_accounts},
    state::{FutarchyAuthority, Market, MarketAsset, YieldAccount, YieldTokenKind},
};

use crate::instructions::common::{
    require_reserve_custody, require_supported_asset_mint, token_account_credit, token_program_for_mint,
    validate_interest_accounts, validate_lp_mint, validate_owner_asset_account, validate_owner_lp_account,
    validate_side_vault_accounts,
};

use super::{reconcile_live_hlp_supply, record_hlp_interest_credit, validate_hlp_authority_pdas};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct WithdrawSingleSidedArgs {
    pub hlp_amount: u64,
    pub min_target_amount_out: u64,
}

#[derive(Accounts)]
#[instruction(args: WithdrawSingleSidedArgs)]
pub struct WithdrawSingleSided<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,

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
    pub borrowed_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner_target_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_hlp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
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
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            target_hlp_mint.key().as_ref(),
            base_mint.key().as_ref(),
            &[YieldTokenKind::Hlp.code()],
        ],
        bump = base_yield_account.bump
    )]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            target_hlp_mint.key().as_ref(),
            quote_mint.key().as_ref(),
            &[YieldTokenKind::Hlp.code()],
        ],
        bump = quote_yield_account.bump
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> WithdrawSingleSided<'info> {
    pub fn validate(&self, args: &WithdrawSingleSidedArgs) -> Result<()> {
        validate_hlp_authority_pdas(
            &self.market,
            self.market.key(),
            &self.futarchy_authority,
            self.futarchy_authority.key(),
        )?;
        self.market.assert_started()?;
        require!(args.hlp_amount > 0, ErrorCode::AmountZero);
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
        let borrowed_asset = target_asset.opposite();
        let borrowed_mint = match borrowed_asset {
            MarketAsset::Base => &self.base_mint,
            MarketAsset::Quote => &self.quote_mint,
        };
        require_keys_eq!(
            self.market.side(target_asset).hlp_mint,
            self.target_hlp_mint.key(),
            ErrorCode::InvalidMint
        );
        let interest_asset = validate_interest_accounts(&self.market, borrowed_mint, &self.borrowed_interest_vault)?;
        require!(interest_asset == borrowed_asset, ErrorCode::InvalidVault);
        validate_owner_asset_account(self.owner.key(), target_mint, &self.owner_target_account)?;
        validate_owner_lp_account(self.owner.key(), &self.target_hlp_mint, &self.owner_hlp_account)?;
        require_gte!(
            self.owner_hlp_account.amount,
            args.hlp_amount,
            ErrorCode::InsufficientBalance
        );
        validate_lp_mint(&self.target_hlp_mint, self.market.key(), target_mint.decimals)?;
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.base_mint.decimals)?;
        self.base_yield_account.assert_account(
            self.owner.key(),
            self.market.key(),
            self.target_hlp_mint.key(),
            self.base_mint.key(),
            YieldTokenKind::Hlp,
        )?;
        self.quote_yield_account.assert_account(
            self.owner.key(),
            self.market.key(),
            self.target_hlp_mint.key(),
            self.quote_mint.key(),
            YieldTokenKind::Hlp,
        )?;
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

    pub fn update_and_validate(&mut self, args: &WithdrawSingleSidedArgs) -> Result<()> {
        self.validate(args)?;
        let target_asset = self.market.asset_for_hlp_mint(self.target_hlp_mint.key())?;
        let current_slot = Clock::get()?.slot;
        self.market.accrue_interest_to_slot(current_slot)?;
        reconcile_live_hlp_supply(&mut self.market, target_asset, self.target_hlp_mint.supply)?;
        self.market.update()?;
        Ok(())
    }

    pub fn handle_withdraw(ctx: Context<'_, '_, '_, 'info, Self>, args: WithdrawSingleSidedArgs) -> Result<()> {
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

        // Checkpoint all yield earned before reducing the owner's hLP balance.
        ctx.accounts.market.checkpoint_hlp_yield_from_ylp(target_asset)?;
        let (base_swap_growth, base_interest_growth) = ctx
            .accounts
            .market
            .hlp_yield_growth_indexes(target_asset, MarketAsset::Base);
        let (quote_swap_growth, quote_interest_growth) = ctx
            .accounts
            .market
            .hlp_yield_growth_indexes(target_asset, MarketAsset::Quote);
        ctx.accounts.base_yield_account.accrue(
            ctx.accounts.owner_hlp_account.amount,
            base_swap_growth,
            base_interest_growth,
        )?;
        ctx.accounts.quote_yield_account.accrue(
            ctx.accounts.owner_hlp_account.amount,
            quote_swap_growth,
            quote_interest_growth,
        )?;

        let hlp_program = token_program_for_mint(
            &ctx.accounts.target_hlp_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        token_burn(
            ctx.accounts.owner.to_account_info(),
            hlp_program,
            ctx.accounts.target_hlp_mint.to_account_info(),
            ctx.accounts.owner_hlp_account.to_account_info(),
            args.hlp_amount,
            &[],
        )?;

        // Settle the hLP debt and credit its closing-period interest.
        let pre_exit_ylp_supply = ctx.accounts.market.base_side.shares.ylp_supply;
        require_eq!(
            pre_exit_ylp_supply,
            ctx.accounts.market.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );
        let receipt = ctx
            .accounts
            .market
            .withdraw_single_sided(target_asset, args.hlp_amount)?;
        if receipt.interest_paid > 0 {
            let interest_vault_balance_before = ctx.accounts.borrowed_interest_vault.amount;
            let borrowed_asset = target_asset.opposite();
            let (borrowed_reserve_vault, borrowed_mint, borrowed_decimals) = match borrowed_asset {
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
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                borrowed_reserve_vault,
                ctx.accounts.borrowed_interest_vault.to_account_info(),
                borrowed_mint,
                borrowed_token_program,
                receipt.interest_paid,
                borrowed_decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            ctx.accounts.borrowed_interest_vault.reload()?;
            let interest_vault_credit =
                token_account_credit(interest_vault_balance_before, &ctx.accounts.borrowed_interest_vault)?;
            let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
            let interest_growth_before = ctx.accounts.market.side(borrowed_asset).fees.interest_growth_index_q64;
            record_hlp_interest_credit(
                ctx.accounts.market.side_mut(borrowed_asset),
                interest_vault_credit,
                manager_fee_bps,
                ctx.accounts.futarchy_authority.revenue_share.interest_bps,
                ctx.accounts.futarchy_authority.protocol_auction_split,
                pre_exit_ylp_supply,
            )?;
            let interest_growth_after = ctx.accounts.market.side(borrowed_asset).fees.interest_growth_index_q64;
            match borrowed_asset {
                MarketAsset::Base => ctx.accounts.base_yield_account.credit_interest_growth(
                    receipt.ylp_amount,
                    interest_growth_after,
                    interest_growth_before,
                )?,
                MarketAsset::Quote => ctx.accounts.quote_yield_account.credit_interest_growth(
                    receipt.ylp_amount,
                    interest_growth_after,
                    interest_growth_before,
                )?,
            }
            // The exiting yLP shares receive their closing-period interest
            // directly above. Checkpoint only the shares that remain in the
            // hLP vault so a burn cannot donate that yield to later holders.
            ctx.accounts.market.checkpoint_hlp_yield_from_ylp(target_asset)?;
        }
        if receipt.hlp_supply == 0 {
            ctx.accounts.market.drain_hlp_unallocated_yield(
                target_asset,
                &mut ctx.accounts.base_yield_account,
                &mut ctx.accounts.quote_yield_account,
            )?;
        }
        let current_slot = Clock::get()?.slot;
        ctx.accounts
            .market
            .finalize_amm_transition_and_observe_risk(current_slot)?;
        ctx.accounts.market.assert_market_health()?;

        // Burn backing yLP and transfer the released target asset to the owner.
        let ylp_program = token_program_for_mint(
            &ctx.accounts.ylp_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        token_burn(
            ctx.accounts.market.to_account_info(),
            ylp_program,
            ctx.accounts.ylp_mint.to_account_info(),
            ctx.accounts.hlp_ylp_account.to_account_info(),
            receipt.ylp_amount,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;

        let target_balance_before = ctx.accounts.owner_target_account.amount;
        let (target_reserve_vault, target_mint, target_decimals) = match target_asset {
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
        let target_token_program = token_program_for_mint(
            match target_asset {
                MarketAsset::Base => &ctx.accounts.base_mint,
                MarketAsset::Quote => &ctx.accounts.quote_mint,
            },
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            target_reserve_vault,
            ctx.accounts.owner_target_account.to_account_info(),
            target_mint,
            target_token_program,
            receipt.target_amount_out,
            target_decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.base_reserve_vault.reload()?;
        ctx.accounts.quote_reserve_vault.reload()?;
        ctx.accounts.owner_target_account.reload()?;
        require_reserve_custody(ctx.accounts.base_reserve_vault.amount, &ctx.accounts.market.base_side)?;
        require_reserve_custody(ctx.accounts.quote_reserve_vault.amount, &ctx.accounts.market.quote_side)?;
        let target_credit = token_account_credit(target_balance_before, &ctx.accounts.owner_target_account)?;
        require_gte!(target_credit, args.min_target_amount_out, ErrorCode::SlippageExceeded);

        // Emit the final hLP position state without an event CPI.
        const MARKET_EVENT_METADATA_LEN: usize = 32 + 32 + 8;
        const HLP_CLOSED_EVENT_LEN: usize = 8 + (3 * 32) + (6 * 8) + MARKET_EVENT_METADATA_LEN;
        let mut data = [0_u8; HLP_CLOSED_EVENT_LEN];
        let mut offset = 0usize;
        data[offset..offset + 8].copy_from_slice(HlpClosed::DISCRIMINATOR);
        offset += 8;
        data[offset..offset + 32].copy_from_slice(market_key.as_ref());
        offset += 32;
        data[offset..offset + 32].copy_from_slice(owner_key.as_ref());
        offset += 32;
        data[offset..offset + 32].copy_from_slice(target_mint_key.as_ref());
        offset += 32;
        data[offset..offset + 8].copy_from_slice(&receipt.hlp_amount.to_le_bytes());
        offset += 8;
        data[offset..offset + 8].copy_from_slice(&receipt.ylp_amount.to_le_bytes());
        offset += 8;
        data[offset..offset + 8].copy_from_slice(&receipt.target_amount_out.to_le_bytes());
        offset += 8;
        data[offset..offset + 8].copy_from_slice(&receipt.debt_repaid.to_le_bytes());
        offset += 8;
        data[offset..offset + 8].copy_from_slice(&receipt.interest_paid.to_le_bytes());
        offset += 8;
        data[offset..offset + 8].copy_from_slice(&receipt.hlp_supply.to_le_bytes());
        offset += 8;
        data[offset..offset + 32].copy_from_slice(owner_key.as_ref());
        offset += 32;
        data[offset..offset + 32].copy_from_slice(market_key.as_ref());
        offset += 32;
        data[offset..offset + 8].copy_from_slice(&current_slot.to_le_bytes());
        sol_log_data(&[&data]);

        Ok(())
    }
}

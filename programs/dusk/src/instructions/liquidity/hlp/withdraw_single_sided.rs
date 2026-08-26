use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::HlpClosed,
    generate_market_seeds,
    state::{FutarchyAuthority, Market, MarketAsset, YieldAccount, YieldTokenKind},
    token::{token_burn, transfer_checked_with_remaining_accounts},
    transitions::HlpYieldEligibility,
};

use crate::instructions::accounts::{
    require_reserve_custody, require_supported_asset_mint, token_account_credit, token_program_for_mint,
    validate_interest_accounts, validate_lp_mint, validate_owner_asset_account, validate_owner_lp_account,
    validate_side_vault_accounts,
};

use super::{
    reconcile_live_hlp_supply, record_hlp_interest_credit, validate_hlp_authority_pdas, validate_hlp_yield_account_pda,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct WithdrawSingleSidedArgs {
    pub hlp_amount: u64,
    pub min_target_amount_out: u64,
}

#[event_cpi]
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

    #[account(mut)]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(mut)]
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
        validate_hlp_yield_account_pda(
            self.base_yield_account.key(),
            self.base_yield_account.bump,
            self.market.key(),
            self.owner.key(),
            self.target_hlp_mint.key(),
            self.base_mint.key(),
        )?;
        self.base_yield_account.assert_account(
            self.owner.key(),
            self.market.key(),
            self.target_hlp_mint.key(),
            self.base_mint.key(),
            YieldTokenKind::Hlp,
        )?;
        validate_hlp_yield_account_pda(
            self.quote_yield_account.key(),
            self.quote_yield_account.bump,
            self.market.key(),
            self.owner.key(),
            self.target_hlp_mint.key(),
            self.quote_mint.key(),
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
        if self.market.hlp_terminally_closed(target_asset) {
            // A retired zero-principal token must remain burnable even if an
            // ordinary hLP settlement/controller checkpoint is unavailable.
            self.market.advance_amm_clock(current_slot)?;
        } else {
            self.market.update()?;
        }
        Ok(())
    }

    pub fn handle_withdraw(ctx: Context<'_, '_, '_, 'info, Self>, args: WithdrawSingleSidedArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let target_asset = ctx
            .accounts
            .market
            .asset_for_hlp_mint(ctx.accounts.target_hlp_mint.key())?;
        let interest_eligibility = HlpYieldEligibility {
            ylp_supply: ctx.accounts.market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: ctx.accounts.market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: ctx.accounts.market.quote_hlp_vault.ylp_shares,
        };
        require_eq!(
            interest_eligibility.ylp_supply,
            ctx.accounts.market.quote_side.shares.ylp_supply,
            ErrorCode::BrokenInvariant
        );

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
            record_hlp_interest_credit(
                &mut ctx.accounts.market,
                borrowed_asset,
                interest_vault_credit,
                ctx.accounts.futarchy_authority.revenue_share.interest_bps,
                ctx.accounts.futarchy_authority.protocol_auction_split,
                interest_eligibility,
            )?;
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
        if receipt.ylp_amount > 0 {
            token_burn(
                ctx.accounts.market.to_account_info(),
                ylp_program,
                ctx.accounts.ylp_mint.to_account_info(),
                ctx.accounts.hlp_ylp_account.to_account_info(),
                receipt.ylp_amount,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
            )?;
        }

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
        if receipt.target_amount_out > 0 {
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
        }
        ctx.accounts.base_reserve_vault.reload()?;
        ctx.accounts.quote_reserve_vault.reload()?;
        ctx.accounts.owner_target_account.reload()?;
        require_reserve_custody(ctx.accounts.base_reserve_vault.amount, &ctx.accounts.market.base_side)?;
        require_reserve_custody(ctx.accounts.quote_reserve_vault.amount, &ctx.accounts.market.quote_side)?;
        let target_credit = token_account_credit(target_balance_before, &ctx.accounts.owner_target_account)?;
        require_gte!(target_credit, args.min_target_amount_out, ErrorCode::SlippageExceeded);

        emit_cpi!(HlpClosed {
            market: market_key,
            owner: owner_key,
            asset_side: target_asset.code(),
            hlp_amount: receipt.hlp_amount,
            ylp_amount: receipt.ylp_amount,
            amount_out: target_credit,
            debt_repaid: receipt.debt_repaid,
            interest_paid: receipt.interest_paid,
            ylp_supply: ctx.accounts.market.base_side.shares.ylp_supply,
            hlp_supply: receipt.hlp_supply,
            base_live_reserve: ctx.accounts.market.base_side.reserves.live_reserve,
            quote_live_reserve: ctx.accounts.market.quote_side.reserves.live_reserve,
        });

        Ok(())
    }
}

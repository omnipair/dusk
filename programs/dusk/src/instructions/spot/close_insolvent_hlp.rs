use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    events::HlpTerminalLiquidated,
    generate_market_seeds,
    instructions::{
        accounts::{
            require_reserve_custody, require_supported_asset_mint, token_account_credit, token_program_for_mint,
            validate_lp_mint, validate_owner_asset_account,
        },
        validate_hlp_authority_pdas,
    },
    state::{FutarchyAuthority, Market, MarketAsset},
    token::{token_burn, transfer_checked_with_remaining_accounts},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseInsolventHlpArgs {
    pub target_asset: u8,
    pub max_socialized_loss: u64,
    pub min_caller_bounty_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
pub struct CloseInsolventHlp<'info> {
    #[account(mut, constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    #[account(mut)]
    pub caller: Signer<'info>,
    pub borrowed_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub borrowed_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub caller_bounty_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub srlp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub target_hlp_srlp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CloseInsolventHlp<'info> {
    pub fn validate(&self, args: &CloseInsolventHlpArgs) -> Result<()> {
        validate_hlp_authority_pdas(
            &self.market,
            self.market.key(),
            &self.futarchy_authority,
            self.futarchy_authority.key(),
        )?;
        self.market.assert_current_version()?;
        self.market.assert_started()?;
        let target_asset = MarketAsset::try_from_code(args.target_asset)?;
        let borrowed_asset = target_asset.opposite();
        let borrowed_side = self.market.side(borrowed_asset);
        let target_vault = match target_asset {
            MarketAsset::Base => &self.market.base_hlp_vault,
            MarketAsset::Quote => &self.market.quote_hlp_vault,
        };
        require_keys_eq!(
            borrowed_side.asset_mint,
            self.borrowed_mint.key(),
            ErrorCode::InvalidMint
        );
        require_keys_eq!(
            borrowed_side.reserve_vault,
            self.borrowed_reserve_vault.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(self.market.srlp_mint, self.srlp_mint.key(), ErrorCode::InvalidLpMintKey);
        require_keys_eq!(
            target_vault.srlp_vault,
            self.target_hlp_srlp_vault.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.borrowed_reserve_vault.mint,
            self.borrowed_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.borrowed_reserve_vault.owner,
            self.market.key(),
            ErrorCode::InvalidVault
        );
        validate_owner_asset_account(self.caller.key(), &self.borrowed_mint, &self.caller_bounty_account)?;
        require_keys_eq!(
            self.target_hlp_srlp_vault.mint,
            self.srlp_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.target_hlp_srlp_vault.owner,
            self.market.key(),
            ErrorCode::InvalidVault
        );
        require_supported_asset_mint(&self.borrowed_mint)?;
        validate_lp_mint(&self.srlp_mint, self.market.key(), self.market.base_side.asset_decimals)
    }

    pub fn update_and_validate(&mut self, args: &CloseInsolventHlpArgs) -> Result<u64> {
        self.validate(args)?;
        let current_slot = Clock::get()?.slot;
        crate::instructions::accounting::accrue_market_interest(
            &mut self.market,
            current_slot,
            self.event_authority.to_account_info(),
            &self.futarchy_authority,
        )?;
        // The ordinary `Market::update` checkpoints hLP settlement and must
        // reject an already-insolvent vault. This terminal path instead
        // advances only the debt and clock state needed to value and close it;
        // the successful waterfall reconstructs the curve and risk below.
        self.market.advance_amm_clock(current_slot)?;
        Ok(current_slot)
    }

    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: CloseInsolventHlpArgs, current_slot: u64) -> Result<()> {
        let target_asset = MarketAsset::try_from_code(args.target_asset)?;
        let borrowed_asset = target_asset.opposite();
        let plan = ctx.accounts.market.prepare_terminal_hlp_waterfall(
            target_asset,
            current_slot,
            Clock::get()?.unix_timestamp,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
        )?;
        require!(plan.target_asset() == target_asset, ErrorCode::BrokenInvariant);
        let borrowed_token_program = token_program_for_mint(
            &ctx.accounts.borrowed_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let receipt = plan.consume(&mut ctx.accounts.market, args.max_socialized_loss)?;
        // consume already rebased the curve for its loss.
        let inventory_after_consume = ctx.accounts.market.ordinary_inventory_point_nad()?;

        if receipt.srlp_burn_amount > 0 {
            token_burn(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.token_2022_program.to_account_info(),
                ctx.accounts.srlp_mint.to_account_info(),
                ctx.accounts.target_hlp_srlp_vault.to_account_info(),
                receipt.srlp_burn_amount,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
            )?;
        }
        // The waterfall paid the bounty from pool cash and absorbed it with
        // the terminal loss, junior first.
        let caller_bounty = receipt.caller_bounty;
        let caller_bounty_before = ctx.accounts.caller_bounty_account.amount;
        if caller_bounty > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.borrowed_reserve_vault.to_account_info(),
                ctx.accounts.caller_bounty_account.to_account_info(),
                ctx.accounts.borrowed_mint.to_account_info(),
                borrowed_token_program.clone(),
                caller_bounty,
                ctx.accounts.borrowed_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            ctx.accounts.caller_bounty_account.reload()?;
            ctx.accounts.borrowed_reserve_vault.reload()?;
        }
        let caller_bounty_credit = token_account_credit(caller_bounty_before, &ctx.accounts.caller_bounty_account)?;
        require_gte!(
            caller_bounty_credit,
            args.min_caller_bounty_out,
            ErrorCode::SlippageExceeded
        );

        // Paid funding interest stays in reserve cash; its protocol share
        // left cash inside the waterfall and is booked here.
        ctx.accounts
            .market
            .book_protocol_interest(ctx.accounts.futarchy_authority.protocol_auction_split)?;
        crate::instructions::accounting::emit_interest_paid(
            &ctx.accounts.market,
            borrowed_asset,
            crate::events::DebtSource::Hlp,
            None,
            crate::state::ReferralInterestQuote::with_protocol_revenue(
                receipt.interest_paid,
                receipt.protocol_paid,
                None,
            )?,
            caller_bounty,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        ctx.accounts
            .market
            .finalize_amm_socialized_loss_and_observe_risk(current_slot, inventory_after_consume)?;
        // This is the terminal recovery path for an already-insolvent hLP.
        // A changed fee on either market mint can leave global health below
        // the floor; requiring that floor here would strand the debt instead
        // of allowing the junior/senior loss waterfall to close it.
        require_reserve_custody(
            ctx.accounts.borrowed_reserve_vault.amount,
            ctx.accounts.market.side(borrowed_asset),
        )?;
        emit_cpi!(HlpTerminalLiquidated {
            market: ctx.accounts.market.key(),
            caller: ctx.accounts.caller.key(),
            target_asset: target_asset.code(),
            debt_closed: receipt.debt_closed,
            srlp_burned: receipt.srlp_burn_amount,
            interest_paid: receipt.interest_paid,
            caller_bounty: caller_bounty_credit,
            socialized_loss: receipt.socialized_loss,
            interest_canceled: receipt.interest_canceled,
            remaining_hlp_supply: receipt.remaining_hlp_supply,
        });
        Ok(())
    }
}

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
            validate_lp_mint,
        },
        record_hlp_interest_credit, validate_hlp_authority_pdas,
    },
    state::{FutarchyAuthority, HlpYieldEligibility, Market, MarketAsset},
    token::{token_burn, transfer_checked_with_remaining_accounts},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseInsolventHlpArgs {
    pub target_asset: u8,
    pub max_insurance_draw: u64,
    pub max_socialized_loss: u64,
}

#[event_cpi]
#[derive(Accounts)]
pub struct CloseInsolventHlp<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,
    #[account(mut)]
    pub caller: Signer<'info>,
    pub borrowed_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub borrowed_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub borrowed_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub ylp_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub target_hlp_ylp_vault: Box<InterfaceAccount<'info, TokenAccount>>,
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
        require_keys_eq!(
            borrowed_side.interest_vault,
            self.borrowed_interest_vault.key(),
            ErrorCode::InvalidVault
        );
        let insurance_key = match borrowed_asset {
            MarketAsset::Base => self.market.insurance.base_vault,
            MarketAsset::Quote => self.market.insurance.quote_vault,
        };
        require_keys_eq!(insurance_key, self.insurance_vault.key(), ErrorCode::InvalidVault);
        require_keys_eq!(self.market.ylp_mint, self.ylp_mint.key(), ErrorCode::InvalidLpMintKey);
        require_keys_eq!(
            target_vault.ylp_vault,
            self.target_hlp_ylp_vault.key(),
            ErrorCode::InvalidVault
        );
        for account in [
            &self.borrowed_reserve_vault,
            &self.borrowed_interest_vault,
            &self.insurance_vault,
        ] {
            require_keys_eq!(account.mint, self.borrowed_mint.key(), ErrorCode::InvalidVault);
            require_keys_eq!(account.owner, self.market.key(), ErrorCode::InvalidVault);
        }
        require_keys_eq!(
            self.target_hlp_ylp_vault.mint,
            self.ylp_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.target_hlp_ylp_vault.owner,
            self.market.key(),
            ErrorCode::InvalidVault
        );
        require_supported_asset_mint(&self.borrowed_mint)?;
        validate_lp_mint(&self.ylp_mint, self.market.key(), self.market.base_side.asset_decimals)
    }

    pub fn update_and_validate(&mut self, args: &CloseInsolventHlpArgs) -> Result<u64> {
        self.validate(args)?;
        let current_slot = Clock::get()?.slot;
        self.market.accrue_interest_to_slot(current_slot)?;
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
        let eligibility = HlpYieldEligibility {
            ylp_supply: ctx.accounts.market.base_side.shares.ylp_supply,
            base_hlp_ylp_shares: ctx.accounts.market.base_hlp_vault.ylp_shares,
            quote_hlp_ylp_shares: ctx.accounts.market.quote_hlp_vault.ylp_shares,
        };
        let plan =
            ctx.accounts
                .market
                .prepare_terminal_hlp_waterfall(target_asset, args.max_insurance_draw, current_slot)?;
        require!(plan.target_asset() == target_asset, ErrorCode::BrokenInvariant);
        let insurance_spent = plan.insurance_request();
        let reserve_before = ctx.accounts.borrowed_reserve_vault.amount;
        let borrowed_token_program = token_program_for_mint(
            &ctx.accounts.borrowed_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        if insurance_spent > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.insurance_vault.to_account_info(),
                ctx.accounts.borrowed_reserve_vault.to_account_info(),
                ctx.accounts.borrowed_mint.to_account_info(),
                borrowed_token_program.clone(),
                insurance_spent,
                ctx.accounts.borrowed_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            ctx.accounts.borrowed_reserve_vault.reload()?;
            ctx.accounts.insurance_vault.reload()?;
        }
        let insurance_credit = token_account_credit(reserve_before, &ctx.accounts.borrowed_reserve_vault)?;
        let receipt = plan.consume(
            &mut ctx.accounts.market,
            insurance_spent,
            insurance_credit,
            args.max_socialized_loss,
        )?;

        if receipt.ylp_burn_amount > 0 {
            token_burn(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.token_2022_program.to_account_info(),
                ctx.accounts.ylp_mint.to_account_info(),
                ctx.accounts.target_hlp_ylp_vault.to_account_info(),
                receipt.ylp_burn_amount,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
            )?;
        }
        let interest_before = ctx.accounts.borrowed_interest_vault.amount;
        if receipt.interest_paid > 0 {
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.borrowed_reserve_vault.to_account_info(),
                ctx.accounts.borrowed_interest_vault.to_account_info(),
                ctx.accounts.borrowed_mint.to_account_info(),
                borrowed_token_program,
                receipt.interest_paid,
                ctx.accounts.borrowed_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                ctx.remaining_accounts,
            )?;
            ctx.accounts.borrowed_interest_vault.reload()?;
            ctx.accounts.borrowed_reserve_vault.reload()?;
            let actual_interest_credit = token_account_credit(interest_before, &ctx.accounts.borrowed_interest_vault)?;
            record_hlp_interest_credit(
                &mut ctx.accounts.market,
                borrowed_asset,
                actual_interest_credit,
                ctx.accounts.futarchy_authority.revenue_share.interest_bps,
                ctx.accounts.futarchy_authority.protocol_auction_split,
                eligibility,
            )?;
        }
        ctx.accounts
            .market
            .finalize_amm_socialized_loss_and_observe_risk(current_slot)?;
        ctx.accounts.market.assert_market_health()?;
        require_reserve_custody(
            ctx.accounts.borrowed_reserve_vault.amount,
            ctx.accounts.market.side(borrowed_asset),
        )?;
        emit_cpi!(HlpTerminalLiquidated {
            market: ctx.accounts.market.key(),
            caller: ctx.accounts.caller.key(),
            target_asset: target_asset.code(),
            debt_closed: receipt.debt_closed,
            ylp_burned: receipt.ylp_burn_amount,
            interest_paid: receipt.interest_paid,
            insurance_drawn: receipt.insurance_drawn,
            socialized_loss: receipt.socialized_loss,
            remaining_hlp_supply: receipt.remaining_hlp_supply,
        });
        Ok(())
    }
}

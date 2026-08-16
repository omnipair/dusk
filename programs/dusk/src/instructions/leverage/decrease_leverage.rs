use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionUpdated, LeverageSwapReceipt, MarketEventMetadata},
    generate_market_seeds,
    market::liquidity::SwapCashPolicy,
    state::{FutarchyAuthority, LeveragePosition, Market, MarketAsset, ReferralAccrual, ReferralPartner},
    token::transfer_checked_with_remaining_accounts,
};

use super::settlement::{
    leverage_swap_fee_credit, prepare_leverage_swap, record_leverage_interest, settle_inline_leverage_hlp,
    validate_leverage_futarchy_pda, validate_leverage_interest_account, validate_leverage_market_pda,
    validate_leverage_mints, validate_leverage_reserve_accounts,
};
use crate::instructions::accounts::{
    require_reserve_custody, token_account_credit, token_program_for_mint, HlpSwapAccountLayout,
};
use crate::instructions::referral::accounting::{referral_interest_accrued_event_at_slot, validate_referral_binding};
use crate::instructions::{enforce_launch_same_transaction_guard, SwapRequest};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct DecreaseLeverageArgs {
    pub debt_asset: u8,
    pub collateral_amount: u64,
    pub min_repay_out: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: DecreaseLeverageArgs)]
pub struct DecreaseLeverage<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,

    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    #[account(address = leverage_position.owner)]
    /// CHECK: Owner address bound by leverage_position.
    pub position_owner: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            leverage_position.position_id.as_ref(),
        ],
        bump = leverage_position.bump,
        constraint = leverage_position.market == market.key() @ ErrorCode::InvalidLeveragePosition,
        constraint = leverage_position.debt_asset == args.debt_asset @ ErrorCode::InvalidLeveragePosition,
    )]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,

    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub debt_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            LEVERAGE_COLLATERAL_VAULT_SEED_PREFIX,
            market.key().as_ref(),
            collateral_mint.key().as_ref(),
        ],
        bump,
        constraint = leverage_collateral_vault.mint == collateral_mint.key() @ ErrorCode::InvalidVault,
        constraint = leverage_collateral_vault.owner == market.key() @ ErrorCode::InvalidVault
    )]
    pub leverage_collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,

    #[account(mut)]
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,

    #[account(mut)]
    pub owner: Signer<'info>,
    /// CHECK: Canonical Instructions sysvar for the launch split guard.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> DecreaseLeverage<'info> {
    pub fn validate_at(&self, args: &DecreaseLeverageArgs, unix_timestamp: i64) -> Result<()> {
        validate_leverage_market_pda(&self.market, self.market.key())?;
        validate_leverage_futarchy_pda(self.futarchy_authority.bump, self.futarchy_authority.key())?;
        self.market.assert_started_at(unix_timestamp)?;
        require_keys_eq!(self.owner.key(), self.position_owner.key(), ErrorCode::InvalidSigner);
        require!(args.collateral_amount > 0, ErrorCode::AmountZero);
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        enforce_launch_same_transaction_guard(
            &self.market,
            self.market.key(),
            debt_asset.opposite(),
            unix_timestamp,
            &self.instructions_sysvar.to_account_info(),
        )?;
        validate_leverage_mints(&self.market, debt_asset, &self.debt_mint, &self.collateral_mint)?;
        validate_leverage_reserve_accounts(
            &self.market,
            debt_asset,
            &self.debt_mint,
            &self.collateral_mint,
            &self.debt_reserve_vault,
            &self.collateral_reserve_vault,
        )?;
        validate_leverage_interest_account(&self.market, &self.debt_mint, &self.debt_interest_vault, debt_asset)?;
        self.leverage_position.require_open()?;
        validate_referral_binding(
            None,
            self.leverage_position.referral_partner,
            self.leverage_position.referral_interest_share_bps,
            true,
            &self.futarchy_authority,
            self.referral_partner.as_deref(),
            self.referral_accrual.as_deref(),
            self.market.key(),
            &self.debt_mint,
        )?;
        Ok(())
    }

    pub fn handle_decrease(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: DecreaseLeverageArgs,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<()> {
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let position_key = ctx.accounts.leverage_position.key();

        // Return position collateral to the reserve and measure its net credit.
        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let collateral_reserve_balance_before = ctx.accounts.collateral_reserve_vault.amount;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            args.collateral_amount,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        let collateral_reserve_credit = token_account_credit(
            collateral_reserve_balance_before,
            &ctx.accounts.collateral_reserve_vault,
        )?;
        require!(collateral_reserve_credit > 0, ErrorCode::AmountZero);

        // Quote the credited collateral as a debt repayment.
        let prepared_swap = prepare_leverage_swap(
            &mut ctx.accounts.market,
            SwapRequest {
                current_slot,
                current_unix_timestamp,
                asset_in: collateral_asset,
                reserve_credit: collateral_reserve_credit,
                protocol_fee_bps: ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            },
            SwapCashPolicy::Decrease {
                debt_asset,
                debt_shares: ctx.accounts.leverage_position.debt_shares,
                debt_principal: ctx.accounts.leverage_position.debt_principal,
            },
        )?;
        let swap = prepared_swap.swap;
        let interest_eligibility = prepared_swap.interest_eligibility;
        let swap_fee_credit = leverage_swap_fee_credit(&swap)?;

        // Commit position accounting and settle hLP exposure and interest.
        let receipt = ctx.accounts.market.decrease_leverage(
            &mut ctx.accounts.leverage_position,
            args.collateral_amount,
            args.min_repay_out,
            prepared_swap,
            swap_fee_credit,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
            current_slot,
            current_unix_timestamp,
        )?;
        settle_inline_leverage_hlp(
            &mut ctx.accounts.market,
            &ctx.accounts.futarchy_authority,
            debt_asset,
            &ctx.accounts.debt_mint,
            &ctx.accounts.collateral_mint,
            &ctx.accounts.debt_reserve_vault,
            &ctx.accounts.collateral_reserve_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            ctx.remaining_accounts,
            h_lp_accounts,
            receipt.base_hlp_rebalance,
            receipt.quote_hlp_rebalance,
            interest_eligibility,
        )?;
        ctx.accounts.debt_interest_vault.reload()?;
        let referral_receipt = record_leverage_interest(
            &mut ctx.accounts.market,
            debt_asset,
            &ctx.accounts.debt_mint,
            &mut ctx.accounts.debt_reserve_vault,
            &mut ctx.accounts.debt_interest_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            &ctx.accounts.futarchy_authority,
            ctx.accounts.leverage_position.referral_partner,
            ctx.accounts.leverage_position.referral_interest_share_bps,
            ctx.accounts.referral_partner.as_deref(),
            ctx.accounts.referral_accrual.as_deref_mut(),
            receipt.interest_paid,
            interest_eligibility,
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;

        // Reconcile physical reserve custody after inline settlement.
        ctx.accounts.debt_reserve_vault.reload()?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        require_reserve_custody(
            ctx.accounts.debt_reserve_vault.amount,
            ctx.accounts.market.side(debt_asset),
        )?;
        require_reserve_custody(
            ctx.accounts.collateral_reserve_vault.amount,
            ctx.accounts.market.side(collateral_asset),
        )?;

        if let Some(event) = referral_interest_accrued_event_at_slot(
            &referral_receipt,
            market_key,
            position_key,
            owner_key,
            owner_key,
            debt_mint_key,
            current_slot,
        )? {
            emit_cpi!(event);
        }

        // Emit the final position state.
        emit_cpi!(LeveragePositionUpdated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            borrowed_amount: receipt.borrowed_amount,
            debt_delta: receipt.debt_delta,
            collateral_delta: receipt.collateral_delta,
            debt_amount: receipt.debt_amount,
            debt_shares: receipt.debt_shares,
            collateral_amount: receipt.collateral_amount,
            closeout_value: receipt.closeout_value,
            owner_credit: 0,
            swap: Some(LeverageSwapReceipt::new(
                swap,
                swap_fee_credit,
                ctx.accounts.market.base_side.reserves.live_reserve,
                ctx.accounts.market.quote_side.reserves.live_reserve,
            )?),
            metadata: MarketEventMetadata::at_slot(owner_key, market_key, current_slot),
        });
        Ok(())
    }
}

use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionUpdated, MarketEventMetadata},
    state::{FutarchyAuthority, LeveragePosition, Market, MarketAsset, ReferralAccrual, ReferralPartner},
    token::{get_transfer_fee_for_epoch, get_transfer_inverse_fee_for_epoch, transfer_checked_with_remaining_accounts},
};

use super::settlement::{record_leverage_interest, validate_leverage_interest_account, validate_owner_debt_account};
use crate::instructions::accounts::{
    require_reserve_custody, require_supported_asset_mint, token_account_credit, token_program_for_mint,
    validate_side_vault_accounts,
};
use crate::instructions::referral::accounting::{referral_interest_accrued_event_at_slot, validate_referral_binding};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct AddLeverageMarginArgs {
    pub debt_asset: u8,
    pub amount: u64,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: AddLeverageMarginArgs)]
pub struct AddLeverageMargin<'info> {
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

    #[account(seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX], bump = futarchy_authority.bump)]
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

    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub debt_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,

    #[account(mut)]
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,

    #[account(mut)]
    pub owner: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> AddLeverageMargin<'info> {
    pub fn validate_at(&self, args: &AddLeverageMarginArgs, unix_timestamp: i64) -> Result<()> {
        self.market.assert_started_at(unix_timestamp)?;
        require_keys_eq!(self.owner.key(), self.position_owner.key(), ErrorCode::InvalidSigner);
        require!(args.amount > 0, ErrorCode::AmountZero);
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        validate_side_vault_accounts(&self.market, debt_asset, &self.debt_mint, &self.debt_reserve_vault)?;
        validate_leverage_interest_account(&self.market, &self.debt_mint, &self.debt_interest_vault, debt_asset)?;
        validate_owner_debt_account(self.owner.key(), &self.debt_mint, &self.owner_debt_account)?;
        require_supported_asset_mint(&self.debt_mint)?;
        require_gte!(
            self.owner_debt_account.amount,
            args.amount,
            ErrorCode::InsufficientBalance
        );
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

    pub fn handle_add_margin(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: AddLeverageMarginArgs,
        current_slot: u64,
        current_epoch: u64,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let position_key = ctx.accounts.leverage_position.key();
        ctx.accounts.market.prepare_leverage_margin_operation(current_slot)?;

        // Resolve the gross transfer required for the exact net debt repayment.
        let reserve_balance_before = ctx.accounts.debt_reserve_vault.amount;
        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let max_repay_credit = args
            .amount
            .checked_sub(get_transfer_fee_for_epoch(
                &ctx.accounts.debt_mint.to_account_info(),
                args.amount,
                current_epoch,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let repay_credit = ctx
            .accounts
            .market
            .debt
            .isolated_repayment_for_max(debt_asset, ctx.accounts.leverage_position.debt_shares, max_repay_credit)?
            .cash_repaid;
        let repay_gross = repay_credit
            .checked_add(get_transfer_inverse_fee_for_epoch(
                &ctx.accounts.debt_mint.to_account_info(),
                repay_credit,
                current_epoch,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        require_gte!(args.amount, repay_gross, ErrorCode::BrokenInvariant);
        transfer_checked_with_remaining_accounts(
            ctx.accounts.owner.to_account_info(),
            ctx.accounts.owner_debt_account.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program,
            repay_gross,
            ctx.accounts.debt_mint.decimals,
            &[],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.debt_reserve_vault.reload()?;
        let measured_repay_credit = token_account_credit(reserve_balance_before, &ctx.accounts.debt_reserve_vault)?;
        require_eq!(measured_repay_credit, repay_credit, ErrorCode::BrokenInvariant);

        // Apply the repayment, route interest, and verify reserve custody.
        let receipt =
            ctx.accounts
                .market
                .add_leverage_margin(&mut ctx.accounts.leverage_position, repay_credit, current_slot)?;
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
            ctx.remaining_accounts,
        )?;
        ctx.accounts.debt_reserve_vault.reload()?;
        require_reserve_custody(
            ctx.accounts.debt_reserve_vault.amount,
            ctx.accounts.market.side(debt_asset),
        )?;

        // Emit referral accrual before the final position state.
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

        emit_cpi!(LeveragePositionUpdated {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: ctx.accounts.market.side(debt_asset.opposite()).asset_mint,
            borrowed_amount: receipt.borrowed_amount,
            debt_delta: receipt.debt_delta,
            collateral_delta: receipt.collateral_delta,
            debt_amount: receipt.debt_amount,
            debt_shares: receipt.debt_shares,
            collateral_amount: receipt.collateral_amount,
            closeout_value: receipt.closeout_value,
            owner_credit: 0,
            swap: None,
            metadata: MarketEventMetadata::at_slot(owner_key, market_key, current_slot),
        });
        Ok(())
    }
}

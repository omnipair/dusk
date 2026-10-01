use crate::transitions::amm::SwapRequest;
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionLiquidated, LeverageSwapReceipt, MarketEventMetadata, SwapExecuted, SwapOrigin},
    generate_market_seeds,
    state::{FutarchyAuthority, LeveragePosition, Market, MarketAsset, ReferralAccrual, ReferralPartner},
    token::{get_transfer_fee, get_transfer_inverse_fee_for_epoch, transfer_checked_with_remaining_accounts},
    transitions::{
        liquidity::SwapCashPolicy, HlpYieldEligibility, LeverageInsuranceDraw, LeverageLiquidationReceipt,
        LeverageSwapFeeCredit,
    },
};

use super::settlement::{
    leverage_collateral_fee, leverage_collateral_liquidation_fee, leverage_collateral_vault_pda, leverage_position_pda,
    leverage_swap_fee_credit, prepare_leverage_swap, record_leverage_interest, settle_inline_leverage_hlp,
    validate_leverage_futarchy_pda, validate_leverage_interest_account, validate_leverage_market_pda,
    validate_leverage_mints, validate_leverage_reserve_accounts, validate_owner_debt_account,
};
use crate::instructions::accounts::{
    require_reserve_custody, token_account_credit, token_program_for_mint, HlpSwapAccountLayout,
};
use crate::instructions::enforce_launch_same_transaction_guard;
use crate::instructions::referral::accounting::{referral_interest_accrued_event_at_slot, validate_referral_binding};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct LiquidateLeveragePositionArgs {
    pub debt_asset: u8,
    /// Minimum debt tokens the liquidator must actually receive after transfer fees.
    pub min_liquidator_amount_out: u64,
}

/// Spend at most the realized residual while targeting the receipt's net
/// liquidator incentive. Transfer fees come out of owner proceeds when there
/// are enough proceeds; an insufficient gross-up still permits liquidation
/// unless the caller requires a larger net minimum.
fn liquidation_payout_debits(
    debt_mint: &AccountInfo,
    residual: u64,
    liquidator_target: u64,
    epoch: u64,
) -> Result<(u64, u64)> {
    let target = liquidator_target.min(residual);
    let gross = if target == 0 {
        0
    } else {
        target
            .saturating_add(get_transfer_inverse_fee_for_epoch(debt_mint, target, epoch)?)
            .min(residual)
    };
    Ok((gross, residual - gross))
}

#[event_cpi]
#[derive(Accounts)]
pub struct LiquidateLeveragePosition<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,

    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    /// CHECK: Receives closed account rent and any non-incentive residual.
    #[account(mut, address = leverage_position.owner)]
    pub position_owner: AccountInfo<'info>,

    #[account(mut, close = position_owner)]
    pub leverage_position: Box<Account<'info, LeveragePosition>>,

    pub debt_mint: Box<InterfaceAccount<'info, Mint>>,
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub debt_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub debt_interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub insurance_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub leverage_collateral_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub liquidator_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut)]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,

    #[account(mut)]
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,

    #[account(mut)]
    pub liquidator: Signer<'info>,
    /// CHECK: Canonical Instructions sysvar for the launch split guard.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> LiquidateLeveragePosition<'info> {
    pub fn validate_at(&self, args: &LiquidateLeveragePositionArgs, unix_timestamp: i64) -> Result<()> {
        validate_leverage_market_pda(&self.market, self.market.key())?;
        validate_leverage_futarchy_pda(self.futarchy_authority.bump, self.futarchy_authority.key())?;
        self.market.assert_started_at(unix_timestamp)?;
        let market_key = self.market.key();
        let (expected_position, expected_position_bump) = leverage_position_pda(
            market_key,
            self.leverage_position.owner,
            self.leverage_position.namespace_authority,
            self.leverage_position.position_id,
        )?;
        require_keys_eq!(
            self.leverage_position.key(),
            expected_position,
            ErrorCode::InvalidLeveragePosition
        );
        require_eq!(
            self.leverage_position.bump,
            expected_position_bump,
            ErrorCode::InvalidLeveragePosition
        );
        require_keys_eq!(
            self.leverage_position.market,
            market_key,
            ErrorCode::InvalidLeveragePosition
        );
        require_eq!(
            self.leverage_position.debt_asset,
            args.debt_asset,
            ErrorCode::InvalidLeveragePosition
        );
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        enforce_launch_same_transaction_guard(
            &self.market,
            market_key,
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
        require_keys_eq!(
            self.insurance_vault.key(),
            match debt_asset {
                MarketAsset::Base => self.market.insurance.base_vault,
                MarketAsset::Quote => self.market.insurance.quote_vault,
            },
            ErrorCode::InvalidVault
        );
        require_keys_eq!(self.insurance_vault.mint, self.debt_mint.key(), ErrorCode::InvalidVault);
        require_keys_eq!(self.insurance_vault.owner, market_key, ErrorCode::InvalidVault);
        let (expected_collateral_vault, _) = leverage_collateral_vault_pda(market_key, self.collateral_mint.key())?;
        require_keys_eq!(
            self.leverage_collateral_vault.key(),
            expected_collateral_vault,
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.leverage_collateral_vault.mint,
            self.collateral_mint.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            self.leverage_collateral_vault.owner,
            market_key,
            ErrorCode::InvalidVault
        );
        validate_owner_debt_account(self.liquidator.key(), &self.debt_mint, &self.liquidator_debt_account)?;
        validate_owner_debt_account(self.position_owner.key(), &self.debt_mint, &self.owner_debt_account)?;
        self.leverage_position.require_open()?;
        validate_referral_binding(
            None,
            self.leverage_position.referral_partner,
            self.leverage_position.referral_interest_share_bps,
            true,
            &self.futarchy_authority,
            self.referral_partner.as_deref(),
            self.referral_accrual.as_deref(),
            market_key,
            &self.debt_mint,
        )?;
        Ok(())
    }

    pub fn handle_liquidate_position(
        mut ctx: Context<'_, '_, '_, 'info, Self>,
        args: LiquidateLeveragePositionArgs,
        current_slot: u64,
        current_unix_timestamp: i64,
    ) -> Result<()> {
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let collateral_sold = ctx.accounts.leverage_position.collateral_amount;
        let effective_unwind_credit = leverage_collateral_fee(&ctx.accounts.collateral_mint, Clock::get()?.epoch)?
            .unwind_credit(collateral_sold)?;
        let admission_unwind_credit =
            leverage_collateral_liquidation_fee(&ctx.accounts.collateral_mint, Clock::get()?.epoch)?
                .unwind_credit(collateral_sold)?;
        let pending_unwind_credit =
            (admission_unwind_credit < effective_unwind_credit).then_some(admission_unwind_credit);

        // Return seized collateral to the reserve and measure its net credit.
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
            collateral_sold,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        let collateral_reserve_credit = token_account_credit(
            collateral_reserve_balance_before,
            &ctx.accounts.collateral_reserve_vault,
        )?;
        crate::instructions::accounting::accrue_market_interest(
            &mut ctx.accounts.market,
            current_slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        // Freeze the swap quote before moving insurance. The measured vault
        // credit, rather than the requested debit, repays the remaining debt.
        let prepared_swap = if collateral_reserve_credit > 0 {
            Some(prepare_leverage_swap(
                &mut ctx.accounts.market,
                SwapRequest {
                    current_slot,
                    current_unix_timestamp,
                    asset_in: collateral_asset,
                    reserve_credit: collateral_reserve_credit,
                    protocol_fee_bps: ctx.accounts.futarchy_authority.revenue_share.swap_bps,
                },
                SwapCashPolicy::Liquidate {
                    debt_asset,
                    debt_shares: ctx.accounts.leverage_position.debt_shares,
                    debt_principal: ctx.accounts.leverage_position.debt_principal,
                    insurance_credit: 0,
                },
            )?)
        } else {
            ctx.accounts.market.prepare_leverage_margin_operation(current_slot)?;
            None
        };
        let interest_eligibility = prepared_swap
            .as_ref()
            .map(|swap| swap.interest_eligibility)
            .unwrap_or_else(|| HlpYieldEligibility {
                ylp_supply: ctx.accounts.market.base_side.shares.ylp_supply,
                base_hlp_ylp_shares: ctx.accounts.market.base_hlp_vault.ylp_shares,
                quote_hlp_ylp_shares: ctx.accounts.market.quote_hlp_vault.ylp_shares,
            });
        let swap_output = prepared_swap
            .as_ref()
            .map(|swap| swap.leverage_quote().amount_out)
            .unwrap_or(0);
        let swap_fee_credit = prepared_swap
            .as_ref()
            .map(|swap| leverage_swap_fee_credit(&swap.leverage_quote()))
            .transpose()?
            .unwrap_or_default();
        let full_repayment = ctx
            .accounts
            .market
            .debt
            .isolated_repayment_for_max(debt_asset, ctx.accounts.leverage_position.debt_shares, u64::MAX)?
            .cash_repaid;
        let insurance_request = ctx
            .accounts
            .market
            .insurance
            .draw_capacity(debt_asset, current_slot)?
            .min(ctx.accounts.insurance_vault.amount)
            .min(full_repayment.saturating_sub(swap_output));
        // A 100% fee cannot repay debt. Preserve the insurance vault and
        // socialize the loss instead of paying the mint's fee authority.
        let insurance_request = if insurance_request > 0
            && get_transfer_fee(&ctx.accounts.debt_mint.to_account_info(), insurance_request)? == insurance_request
        {
            0
        } else {
            insurance_request
        };
        let insurance = if insurance_request > 0 {
            let debt_token_program = token_program_for_mint(
                &ctx.accounts.debt_mint,
                &ctx.accounts.token_program,
                &ctx.accounts.token_2022_program,
            )?;
            let insurance_before = ctx.accounts.insurance_vault.amount;
            let reserve_before = ctx.accounts.debt_reserve_vault.amount;
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.insurance_vault.to_account_info(),
                ctx.accounts.debt_reserve_vault.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                debt_token_program,
                insurance_request,
                ctx.accounts.debt_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
            )?;
            ctx.accounts.insurance_vault.reload()?;
            ctx.accounts.debt_reserve_vault.reload()?;
            let spent = insurance_before
                .checked_sub(ctx.accounts.insurance_vault.amount)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require_eq!(spent, insurance_request, ErrorCode::BrokenInvariant);
            LeverageInsuranceDraw {
                spent,
                credit: token_account_credit(reserve_before, &ctx.accounts.debt_reserve_vault)?,
            }
        } else {
            LeverageInsuranceDraw::default()
        };

        // Commit liquidation accounting and settle the resulting hLP exposure.
        let receipt = ctx.accounts.market.liquidate_leverage_position_with_pending_credit(
            &mut ctx.accounts.leverage_position,
            prepared_swap,
            collateral_reserve_credit,
            pending_unwind_credit,
            swap_fee_credit,
            insurance,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
            current_slot,
        )?;
        finish_liquidation(
            &mut ctx,
            debt_asset,
            &receipt,
            swap_fee_credit,
            interest_eligibility,
            current_slot,
            args.min_liquidator_amount_out,
        )
    }
}

/// Keeps token settlement and event construction out of the already-large
/// liquidation quote frame. This is a stack-only SBF refactor; all ordering
/// and accounting remain identical to the handler path.
#[inline(never)]
fn finish_liquidation<'info>(
    ctx: &mut Context<'_, '_, '_, 'info, LiquidateLeveragePosition<'info>>,
    debt_asset: MarketAsset,
    receipt: &LeverageLiquidationReceipt,
    swap_fee_credit: LeverageSwapFeeCredit,
    interest_eligibility: HlpYieldEligibility,
    current_slot: u64,
    min_liquidator_amount_out: u64,
) -> Result<()> {
    let market_key = ctx.accounts.market.key();
    let liquidator_key = ctx.accounts.liquidator.key();
    let owner_key = ctx.accounts.position_owner.key();
    let debt_mint_key = ctx.accounts.debt_mint.key();
    let collateral_mint_key = ctx.accounts.collateral_mint.key();
    let position_key = ctx.accounts.leverage_position.key();
    let expected_referral_partner = ctx.accounts.leverage_position.referral_partner;
    let collateral_asset = debt_asset.opposite();
    let h_lp_accounts = {
        let market: &Market = &ctx.accounts.market;
        HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
    };

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
        ctx.accounts.event_authority.to_account_info(),
    )?;
    ctx.accounts.debt_interest_vault.reload()?;

    // Pay the liquidator a net incentive when available, then return any
    // residual to the owner. The measured credit enforces the caller's floor.
    let debt_token_program = token_program_for_mint(
        &ctx.accounts.debt_mint,
        &ctx.accounts.token_program,
        &ctx.accounts.token_2022_program,
    )?;
    let residual = receipt
        .liquidator_amount
        .checked_add(receipt.owner_residual)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let (liquidator_debit, owner_debit) = liquidation_payout_debits(
        &ctx.accounts.debt_mint.to_account_info(),
        residual,
        receipt.liquidator_amount,
        Clock::get()?.epoch,
    )?;
    let liquidator_balance_before = ctx.accounts.liquidator_debt_account.amount;
    transfer_checked_with_remaining_accounts(
        ctx.accounts.market.to_account_info(),
        ctx.accounts.debt_reserve_vault.to_account_info(),
        ctx.accounts.liquidator_debt_account.to_account_info(),
        ctx.accounts.debt_mint.to_account_info(),
        debt_token_program.clone(),
        liquidator_debit,
        ctx.accounts.debt_mint.decimals,
        &[&generate_market_seeds!(ctx.accounts.market)[..]],
        h_lp_accounts.hook_accounts(ctx.remaining_accounts),
    )?;
    ctx.accounts.liquidator_debt_account.reload()?;
    let liquidator_amount = token_account_credit(liquidator_balance_before, &ctx.accounts.liquidator_debt_account)?;
    require_gte!(
        liquidator_amount,
        min_liquidator_amount_out,
        ErrorCode::SlippageExceeded
    );

    let owner_balance_before = ctx.accounts.owner_debt_account.amount;
    transfer_checked_with_remaining_accounts(
        ctx.accounts.market.to_account_info(),
        ctx.accounts.debt_reserve_vault.to_account_info(),
        ctx.accounts.owner_debt_account.to_account_info(),
        ctx.accounts.debt_mint.to_account_info(),
        debt_token_program,
        owner_debit,
        ctx.accounts.debt_mint.decimals,
        &[&generate_market_seeds!(ctx.accounts.market)[..]],
        h_lp_accounts.hook_accounts(ctx.remaining_accounts),
    )?;
    ctx.accounts.owner_debt_account.reload()?;
    let owner_residual = token_account_credit(owner_balance_before, &ctx.accounts.owner_debt_account)?;

    // Route accrued interest and reconcile physical reserve custody.
    let referral_receipt = record_leverage_interest(
        &mut ctx.accounts.market,
        debt_asset,
        &ctx.accounts.debt_mint,
        &mut ctx.accounts.debt_reserve_vault,
        &mut ctx.accounts.debt_interest_vault,
        &ctx.accounts.token_program,
        &ctx.accounts.token_2022_program,
        &ctx.accounts.futarchy_authority,
        expected_referral_partner,
        ctx.accounts.leverage_position.referral_interest_share_bps,
        ctx.accounts.referral_partner.as_deref(),
        ctx.accounts.referral_accrual.as_deref_mut(),
        receipt.interest_paid,
        interest_eligibility,
        h_lp_accounts.hook_accounts(ctx.remaining_accounts),
    )?;
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

    crate::instructions::accounting::emit_interest_paid(
        &ctx.accounts.market,
        debt_asset,
        crate::events::DebtSource::Margin,
        Some(position_key),
        referral_receipt.quote,
        0,
        ctx.accounts.event_authority.to_account_info(),
    )?;
    if let Some(event) = referral_interest_accrued_event_at_slot(
        &referral_receipt,
        market_key,
        position_key,
        owner_key,
        liquidator_key,
        debt_mint_key,
        current_slot,
    )? {
        emit_cpi!(event);
    }

    // Emit the final liquidation state.
    let swap_event = LeverageSwapReceipt::new(
        receipt.swap,
        swap_fee_credit,
        ctx.accounts.market.base_side.reserves.live_reserve,
        ctx.accounts.market.quote_side.reserves.live_reserve,
    )?;
    if receipt.swap.amount_in > 0 {
        emit_cpi!(SwapExecuted::from_leverage(
            market_key,
            owner_key,
            liquidator_key,
            position_key,
            SwapOrigin::LeverageLiquidation,
            current_slot,
            swap_event,
            receipt.swap.start_price_nad,
            &ctx.accounts.market,
        )?);
    }
    emit_cpi!(LeveragePositionLiquidated {
        market: market_key,
        position: position_key,
        owner: owner_key,
        liquidator: liquidator_key,
        debt_asset_mint: debt_mint_key,
        collateral_asset_mint: collateral_mint_key,
        debt_repaid: receipt.debt_repaid,
        insurance_drawn: receipt.insurance_drawn,
        socialized_loss: receipt.socialized_loss,
        interest_paid: receipt.interest_paid,
        principal_written_off: receipt.principal_written_off,
        collateral_sold: receipt.collateral_sold,
        closeout_value: receipt.closeout_value,
        liquidator_amount,
        owner_residual,
        swap: swap_event,
        metadata: MarketEventMetadata::at_slot(liquidator_key, market_key, current_slot),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/leverage/liquidate_leverage_position.rs");
}

use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;

use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    events::{LeverageSwapReceipt, SwapExecuted, SwapOrigin},
    generate_market_seeds,
    instructions::{
        accounts::{require_reserve_custody, token_account_info_amount, token_program_for_mint, HlpSwapAccountLayout},
        flash_liquidation::*,
        leverage::settlement::{record_leverage_interest, settle_inline_leverage_hlp},
        leverage_collateral_fee, leverage_collateral_liquidation_fee,
    },
    state::*,
    token::transfer_checked_with_remaining_accounts,
    transitions::{
        amm_liquidation::EmergencyLiquidationQuote, flash_settlement::LiquidationPositionMut, HlpYieldEligibility,
        LeverageSwapFeeCredit,
    },
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct EmergencyLiquidationArgs {
    pub debt_asset: u8,
    pub collateral_debit: u64,
    pub full: bool,
    pub min_reward_credit: u64,
    pub min_swap_output: u64,
}

#[event_cpi]
#[derive(Accounts)]
pub struct EmergencyLiquidation<'info> {
    pub accounts: FlashLiquidationAccounts<'info>,
    #[account(mut)]
    pub collateral_reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: Canonical transaction instructions for the launch guard.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
}

#[event]
pub struct EmergencyLiquidationSettled {
    pub market: Pubkey,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub caller: Pubkey,
    pub leverage: bool,
    pub debt_asset: u8,
    pub quote: EmergencyLiquidationQuote,
    pub remaining_collateral: u64,
    pub remaining_debt: u64,
}

impl<'info> EmergencyLiquidation<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, args: EmergencyLiquidationArgs) -> Result<()> {
        let clock = Clock::get()?;
        let asset = MarketAsset::try_from_code(args.debt_asset)?;
        let a = &mut ctx.accounts.accounts;
        crate::instructions::enforce_launch_same_transaction_guard(
            &a.market,
            a.market.key(),
            asset.opposite(),
            clock.unix_timestamp,
            &ctx.accounts.instructions,
        )?;
        crate::instructions::accounting::accrue_market_interest(
            &mut a.market,
            clock.slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        a.market.update()?;
        a.validate(asset)?;
        a.position()?.require_idle()?;
        require_keys_eq!(
            ctx.accounts.collateral_reserve_vault.key(),
            a.market.side(asset.opposite()).reserve_vault,
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            ctx.accounts.collateral_reserve_vault.owner,
            a.market.key(),
            ErrorCode::InvalidVault
        );
        require_keys_eq!(
            ctx.accounts.collateral_reserve_vault.mint,
            a.collateral_mint.key(),
            ErrorCode::InvalidMint
        );
        let collateral_fee = leverage_collateral_fee(&a.collateral_mint, clock.epoch)?;
        let eligibility_fee = leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?;
        let debt_fee = leverage_collateral_fee(&a.debt_mint, clock.epoch)?;
        let health = a
            .market
            .flash_liquidation_health(&a.position()?, asset, eligibility_fee)?;
        require!(health.emergency_allowed()?, ErrorCode::PositionNotLiquidatable);
        match (&mut a.borrow_position, &mut a.leverage_position) {
            (Some(p), None) => p.distress_mut(asset).observe(true, clock.unix_timestamp, clock.slot)?,
            (None, Some(p)) => p.distress.observe(true, clock.unix_timestamp, clock.slot)?,
            _ => return err!(ErrorCode::InvalidLiquidationSession),
        }
        let mut prepared = match (&a.borrow_position, &a.leverage_position) {
            (Some(p), None) => a.market.prepare_emergency_liquidation(
                &crate::transitions::flash_liquidation::LiquidationPositionRef::Borrow(p),
                asset,
                args.collateral_debit,
                args.full,
                collateral_fee,
                eligibility_fee,
                debt_fee,
                clock.unix_timestamp,
                clock.slot,
                a.futarchy_authority.revenue_share.swap_bps,
            )?,
            (None, Some(p)) => a.market.prepare_emergency_liquidation(
                &crate::transitions::flash_liquidation::LiquidationPositionRef::Leverage(p),
                asset,
                args.collateral_debit,
                args.full,
                collateral_fee,
                eligibility_fee,
                debt_fee,
                clock.unix_timestamp,
                clock.slot,
                a.futarchy_authority.revenue_share.swap_bps,
            )?,
            _ => return err!(ErrorCode::InvalidLiquidationSession),
        };
        let q = prepared.quote;
        require_gte!(q.reward_credit, args.min_reward_credit, ErrorCode::SlippageExceeded);
        require_gte!(q.swap_output, args.min_swap_output, ErrorCode::SlippageExceeded);
        let eligibility = prepared
            .swap
            .as_ref()
            .map(|s| s.interest_eligibility)
            .unwrap_or(HlpYieldEligibility {
                ylp_supply: a.market.base_side.shares.ylp_supply,
                base_hlp_ylp_shares: a.market.base_hlp_vault.ylp_shares,
                quote_hlp_ylp_shares: a.market.quote_hlp_vault.ylp_shares,
            });
        let swap_quote = prepared.swap.as_ref().map(|s| s.leverage_quote());
        let (partner, share) = a.referral(asset)?;
        let layout = HlpSwapAccountLayout::try_from((&**a.market, ctx.remaining_accounts))?;
        let hooks = layout.hook_accounts(ctx.remaining_accounts);
        let collateral_before = ctx.accounts.collateral_reserve_vault.amount;
        transfer_checked_with_remaining_accounts(
            a.market.to_account_info(),
            a.collateral_vault.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            a.collateral_mint.to_account_info(),
            token_program_for_mint(&a.collateral_mint, &a.token_program, &a.token_2022_program)?,
            q.collateral_debit,
            a.collateral_mint.decimals,
            &[&generate_market_seeds!(a.market)[..]],
            hooks,
        )?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        require_eq!(
            ctx.accounts
                .collateral_reserve_vault
                .amount
                .checked_sub(collateral_before)
                .ok_or(ErrorCode::BrokenInvariant)?,
            q.collateral_credit,
            ErrorCode::BrokenInvariant
        );
        if q.insurance_debit > 0 {
            let before = a.reserve_vault.amount;
            transfer_checked_with_remaining_accounts(
                a.market.to_account_info(),
                a.insurance_vault.to_account_info(),
                a.reserve_vault.to_account_info(),
                a.debt_mint.to_account_info(),
                a.debt_token_program.to_account_info(),
                q.insurance_debit,
                a.debt_mint.decimals,
                &[&generate_market_seeds!(a.market)[..]],
                hooks,
            )?;
            a.reserve_vault.reload()?;
            require_eq!(
                a.reserve_vault
                    .amount
                    .checked_sub(before)
                    .ok_or(ErrorCode::BrokenInvariant)?,
                q.loss.insurance_credit,
                ErrorCode::BrokenInvariant
            );
        }
        let finalized = {
            let mut position = match (&mut a.borrow_position, &mut a.leverage_position) {
                (Some(p), None) => LiquidationPositionMut::Borrow(p),
                (None, Some(p)) => LiquidationPositionMut::Leverage(p),
                _ => return err!(ErrorCode::InvalidLiquidationSession),
            };
            a.market.apply_emergency_liquidation(
                &mut position,
                asset,
                &mut prepared,
                eligibility_fee,
                clock.unix_timestamp,
                clock.slot,
                a.futarchy_authority.revenue_share.swap_bps,
                a.futarchy_authority.protocol_auction_split,
            )?
        };
        if let Some(finalized) = finalized {
            settle_inline_leverage_hlp(
                &mut a.market,
                &a.futarchy_authority,
                asset,
                &a.debt_mint,
                &a.collateral_mint,
                &a.reserve_vault,
                &ctx.accounts.collateral_reserve_vault,
                &a.token_program,
                &a.token_2022_program,
                ctx.remaining_accounts,
                layout,
                finalized.base_rebalance,
                finalized.quote_rebalance,
                eligibility,
                ctx.accounts.event_authority.to_account_info(),
            )?;
        }
        for (destination, debit, credit) in [
            (
                a.buyer_refund_account.to_account_info(),
                q.reward_debit,
                q.reward_credit,
            ),
            (a.owner_debt_account.to_account_info(), q.owner_debit, q.owner_credit),
            (
                a.insurance_vault.to_account_info(),
                q.insurance_fee_debit,
                q.fee.insurance,
            ),
            (
                a.interest_vault.to_account_info(),
                q.revenue_fee_debit,
                q.fee.lp + q.fee.protocol,
            ),
        ] {
            if debit == 0 {
                continue;
            }
            let before = token_account_info_amount(&destination)?;
            transfer_checked_with_remaining_accounts(
                a.market.to_account_info(),
                a.reserve_vault.to_account_info(),
                destination.clone(),
                a.debt_mint.to_account_info(),
                a.debt_token_program.to_account_info(),
                debit,
                a.debt_mint.decimals,
                &[&generate_market_seeds!(a.market)[..]],
                hooks,
            )?;
            require_eq!(
                token_account_info_amount(&destination)?
                    .checked_sub(before)
                    .ok_or(ErrorCode::BrokenInvariant)?,
                credit,
                ErrorCode::BrokenInvariant
            );
        }
        if q.fee.insurance > 0 {
            a.market.insurance.credit(asset, q.fee.insurance, clock.slot)?;
        }
        a.market.side_mut(asset).record_liquidation_fee_credit(
            q.fee.lp,
            q.fee.protocol,
            a.futarchy_authority.protocol_auction_split,
            eligibility.ylp_supply,
        )?;
        a.interest_vault.reload()?;
        let referral_receipt = record_leverage_interest(
            &mut a.market,
            asset,
            &a.debt_mint,
            &mut a.reserve_vault,
            &mut a.interest_vault,
            &a.token_program,
            &a.token_2022_program,
            &a.futarchy_authority,
            partner,
            share,
            a.referral_partner.as_deref(),
            a.referral_accrual.as_deref_mut(),
            q.loss.interest_paid,
            eligibility,
            hooks,
        )?;
        a.market
            .checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Base, eligibility.base_hlp_ylp_shares)?;
        a.market
            .checkpoint_hlp_yield_from_ylp_shares(MarketAsset::Quote, eligibility.quote_hlp_ylp_shares)?;
        super::flash_liquidation::emit_liquidation_interest(
            a,
            asset,
            &referral_receipt,
            ctx.accounts.event_authority.to_account_info(),
            clock.slot,
        )?;
        a.reserve_vault.reload()?;
        a.insurance_vault.reload()?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        require_reserve_custody(a.reserve_vault.amount, a.market.side(asset))?;
        require_reserve_custody(
            ctx.accounts.collateral_reserve_vault.amount,
            a.market.side(asset.opposite()),
        )?;
        require_gte!(
            a.insurance_vault.amount,
            a.market.insurance.available(asset),
            ErrorCode::UnbackedFeeLiability
        );
        require_gte!(
            a.interest_vault.amount,
            a.market.side(asset).fees.interest_vault_balance,
            ErrorCode::UnbackedFeeLiability
        );
        let position_key = a.position_key()?;
        let owner = a.owner()?;
        if let Some(swap) = swap_quote {
            let receipt = LeverageSwapReceipt::new(
                swap,
                LeverageSwapFeeCredit::from_total_actual_credit(&swap, swap.fee_breakdown.claimable_fee_debit)?,
                a.market.base_side.reserves.live_reserve,
                a.market.quote_side.reserves.live_reserve,
            )?;
            emit_cpi!(SwapExecuted::from_leverage(
                a.market.key(),
                owner,
                a.buyer.key(),
                position_key,
                if a.leverage_position.is_some() {
                    SwapOrigin::LeverageLiquidation
                } else {
                    SwapOrigin::CreditLiquidation
                },
                clock.slot,
                receipt,
                swap.start_price_nad,
                &a.market
            )?);
        }
        emit_cpi!(EmergencyLiquidationSettled {
            market: a.market.key(),
            position: position_key,
            owner,
            caller: a.buyer.key(),
            leverage: a.leverage_position.is_some(),
            debt_asset: asset.code(),
            quote: q,
            remaining_collateral: a.position()?.collateral(asset),
            remaining_debt: prepared.allocation.remaining_debt
        });
        Ok(())
    }
}

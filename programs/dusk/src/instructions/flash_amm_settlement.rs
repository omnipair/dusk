use super::flash_liquidation::{FlashLiquidationSettled, SettleFlashLiquidation};
use crate::{
    constants::MARKET_V2_SEED_PREFIX,
    errors::ErrorCode,
    events::{LeverageSwapReceipt, SwapExecuted, SwapOrigin},
    generate_market_seeds,
    instructions::{
        accounts::{require_reserve_custody, token_account_info_amount, token_program_for_mint, HlpSwapAccountLayout},
        leverage::settlement::{record_leverage_interest, settle_inline_leverage_hlp},
        leverage_collateral_fee, leverage_collateral_liquidation_fee,
    },
    state::*,
    token::transfer_checked_with_remaining_accounts,
    transitions::{
        amm::SwapRequest,
        flash_settlement::{LiquidationDebtAllocation, LiquidationPositionMut},
        liquidity::SwapCashPolicy,
        LeverageSwapFeeCredit,
    },
};
use anchor_lang::prelude::*;

/// The session has already been validated by the common settle entrypoint.
/// This is an alternate source of its fixed payment, not a new price policy.
#[inline(never)]
pub(super) fn settle_with_amm<'info>(
    ctx: &mut Context<'_, '_, '_, 'info, SettleFlashLiquidation<'info>>,
    asset: MarketAsset,
    allocation: LiquidationDebtAllocation,
    repayment: u64,
    fee: LiquidationFeeAllocation,
    owner_surplus: u64,
    insurance_debit: u64,
    loss: LiquidationLossAllocation,
    clock: Clock,
) -> Result<()> {
    let a = &mut ctx.accounts.accounts;
    let session = &ctx.accounts.session;
    let q = session.quote;
    let collateral_reserve = ctx
        .accounts
        .collateral_reserve_vault
        .as_mut()
        .ok_or(ErrorCode::InvalidVault)?;
    require_keys_eq!(
        collateral_reserve.key(),
        a.market.side(asset.opposite()).reserve_vault,
        ErrorCode::InvalidVault
    );
    require_keys_eq!(collateral_reserve.owner, a.market.key(), ErrorCode::InvalidVault);
    require_keys_eq!(collateral_reserve.mint, a.collateral_mint.key(), ErrorCode::InvalidMint);
    crate::instructions::enforce_launch_same_transaction_guard(
        &a.market,
        a.market.key(),
        asset.opposite(),
        clock.unix_timestamp,
        &ctx.accounts.instructions,
    )?;
    let collateral_credit =
        leverage_collateral_fee(&a.collateral_mint, clock.epoch)?.unwind_credit(q.collateral_credit)?;
    require_gt!(collateral_credit, 0, ErrorCode::InsufficientAmount);
    let debt_fee = leverage_collateral_fee(&a.debt_mint, clock.epoch)?;
    let insurance_fee_debit = debt_fee.effective_gross_for_credit(fee.insurance)?;
    let revenue_fee_debit = debt_fee.effective_gross_for_credit(fee.lp + fee.protocol)?;
    let owner_debit = debt_fee.effective_gross_for_credit(owner_surplus)?;
    let obligation = repayment
        .checked_add(insurance_fee_debit)
        .and_then(|v| v.checked_add(revenue_fee_debit))
        .and_then(|v| v.checked_add(owner_debit))
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let mut swap = SwapRequest {
        current_slot: clock.slot,
        current_unix_timestamp: clock.unix_timestamp,
        asset_in: asset.opposite(),
        reserve_credit: collateral_credit,
        protocol_fee_bps: a.futarchy_authority.revenue_share.swap_bps,
    }
    .prepare_with_cash_policy(&mut a.market, SwapCashPolicy::LiquidationQuote)?;
    require_gte!(swap.quote.amount_out, obligation, ErrorCode::InsufficientAmount);
    let buyer_debit = swap.quote.amount_out - obligation;
    let buyer_credit = debt_fee.unwind_credit(buyer_debit)?;
    let policy = SwapCashPolicy::SettleLiquidation {
        debt_asset: asset,
        isolated: session.leverage,
        full: q.full,
        shares: allocation.shares,
        principal_removed: allocation.principal,
        debt_reduced: allocation.debt,
        repayment,
        insurance_credit: loss.insurance_credit,
    };
    swap.bind_liquidation(&a.market, policy)?;
    let swap_quote = swap.leverage_quote();
    let swap_fee = LeverageSwapFeeCredit::from_total_actual_credit(&swap_quote, swap.quote.fee.claimable_fee_debit)?;
    let eligibility = swap.interest_eligibility;
    let (partner, share) = a.referral(asset)?;
    let layout = HlpSwapAccountLayout::try_from((&**a.market, ctx.remaining_accounts))?;
    let hooks = layout.hook_accounts(ctx.remaining_accounts);
    let before = collateral_reserve.amount;
    transfer_checked_with_remaining_accounts(
        a.buyer.to_account_info(),
        a.buyer_collateral_account.to_account_info(),
        collateral_reserve.to_account_info(),
        a.collateral_mint.to_account_info(),
        token_program_for_mint(&a.collateral_mint, &a.token_program, &a.token_2022_program)?,
        q.collateral_credit,
        a.collateral_mint.decimals,
        &[],
        hooks,
    )?;
    collateral_reserve.reload()?;
    require_eq!(
        collateral_reserve
            .amount
            .checked_sub(before)
            .ok_or(ErrorCode::BrokenInvariant)?,
        collateral_credit,
        ErrorCode::BrokenInvariant
    );
    if insurance_debit > 0 {
        let before = a.reserve_vault.amount;
        transfer_checked_with_remaining_accounts(
            a.market.to_account_info(),
            a.insurance_vault.to_account_info(),
            a.reserve_vault.to_account_info(),
            a.debt_mint.to_account_info(),
            a.debt_token_program.to_account_info(),
            insurance_debit,
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
            loss.insurance_credit,
            ErrorCode::BrokenInvariant
        );
    }
    let finalized = swap.apply(
        &mut a.market,
        policy,
        swap_fee,
        clock.slot,
        a.futarchy_authority.revenue_share.swap_bps,
        a.futarchy_authority.protocol_auction_split,
        None,
    )?;
    if insurance_debit > 0 {
        a.market.insurance.consume_draw(asset, insurance_debit, clock.slot)?;
    }
    {
        let mut position = match (&mut a.borrow_position, &mut a.leverage_position) {
            (Some(p), None) => LiquidationPositionMut::Borrow(p),
            (None, Some(p)) => LiquidationPositionMut::Leverage(p),
            _ => return err!(ErrorCode::InvalidLiquidationSession),
        };
        a.market
            .apply_liquidation_position(&mut position, asset, allocation, q.collateral_debit, q.full)?;
        a.market.observe_liquidation_settlement(
            &mut position,
            asset,
            allocation.remaining_debt,
            q.full,
            leverage_collateral_liquidation_fee(&a.collateral_mint, clock.epoch)?,
            clock.unix_timestamp,
            clock.slot,
        )?;
    }
    settle_inline_leverage_hlp(
        &mut a.market,
        &a.futarchy_authority,
        asset,
        &a.debt_mint,
        &a.collateral_mint,
        &a.reserve_vault,
        collateral_reserve,
        &a.token_program,
        &a.token_2022_program,
        ctx.remaining_accounts,
        layout,
        finalized.base_rebalance,
        finalized.quote_rebalance,
        eligibility,
        ctx.accounts.event_authority.to_account_info(),
    )?;
    for (destination, debit, credit) in [
        (a.buyer_refund_account.to_account_info(), buyer_debit, buyer_credit),
        (a.owner_debt_account.to_account_info(), owner_debit, owner_surplus),
        (a.insurance_vault.to_account_info(), insurance_fee_debit, fee.insurance),
        (
            a.interest_vault.to_account_info(),
            revenue_fee_debit,
            fee.lp + fee.protocol,
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
    if fee.insurance > 0 {
        a.market.insurance.credit(asset, fee.insurance, clock.slot)?;
    }
    a.market.side_mut(asset).record_liquidation_fee_credit(
        fee.lp,
        fee.protocol,
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
        loss.interest_paid,
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
    let refund = ctx
        .accounts
        .repayment_vault
        .amount
        .checked_sub(session.payment_balance_before)
        .ok_or(ErrorCode::BrokenInvariant)?;
    if refund > 0 {
        transfer_checked_with_remaining_accounts(
            session.to_account_info(),
            ctx.accounts.repayment_vault.to_account_info(),
            a.buyer_refund_account.to_account_info(),
            a.debt_mint.to_account_info(),
            a.debt_token_program.to_account_info(),
            refund,
            a.debt_mint.decimals,
            &[&[LIQUIDATION_SESSION_SEED, session.position.as_ref(), &[session.bump]]],
            hooks,
        )?;
    }
    a.reserve_vault.reload()?;
    collateral_reserve.reload()?;
    a.insurance_vault.reload()?;
    require_reserve_custody(a.reserve_vault.amount, a.market.side(asset))?;
    require_reserve_custody(collateral_reserve.amount, a.market.side(asset.opposite()))?;
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
    match (&mut a.borrow_position, &mut a.leverage_position) {
        (Some(p), None) => p.active_liquidation_session = Pubkey::default(),
        (None, Some(p)) => p.active_liquidation_session = Pubkey::default(),
        _ => return err!(ErrorCode::InvalidLiquidationSession),
    }
    emit_cpi!(SwapExecuted::from_leverage(
        a.market.key(),
        session.owner,
        a.buyer.key(),
        session.position,
        if session.leverage {
            SwapOrigin::LeverageLiquidation
        } else {
            SwapOrigin::CreditLiquidation
        },
        clock.slot,
        LeverageSwapReceipt::new(
            swap_quote,
            swap_fee,
            a.market.base_side.reserves.live_reserve,
            a.market.quote_side.reserves.live_reserve
        )?,
        swap_quote.start_price_nad,
        &a.market
    )?);
    emit_cpi!(FlashLiquidationSettled {
        market: a.market.key(),
        position: session.position,
        buyer: a.buyer.key(),
        leverage: session.leverage,
        debt_asset: asset.code(),
        collateral_debit: q.collateral_debit,
        debt_repaid: repayment,
        owner_surplus,
        fee,
        loss,
        remaining_collateral: a.position()?.collateral(asset),
        remaining_debt: allocation.remaining_debt
    });
    ctx.accounts.session.close(a.buyer.to_account_info())?;
    Ok(())
}

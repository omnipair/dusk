use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use super::settlement::{
    invoke_delegated_approval_callback, leverage_collateral_credit, leverage_swap_fee_credit, prepare_leverage_swap,
    record_leverage_interest, settle_inline_leverage_hlp, split_delegated_accounts, validate_leverage_futarchy_pda,
    validate_leverage_interest_account, validate_leverage_market_pda, validate_leverage_mints,
    validate_leverage_reserve_accounts, DelegatedCpiArgs, LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_CLOSE_SETTLED,
};
use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionClosed, LeveragePositionUpdated, LeverageSwapReceipt, MarketEventMetadata},
    generate_market_seeds,
    instructions::{
        accounts::{require_reserve_custody, token_account_credit, token_program_for_mint, HlpSwapAccountLayout},
        enforce_launch_same_transaction_guard,
        referral::accounting::{referral_interest_accrued_event_at_slot, validate_referral_binding},
        SwapRequest,
    },
    state::{
        FutarchyAuthority, LeverageDelegation, LeveragePosition, Market, MarketAsset, ReferralAccrual, ReferralPartner,
    },
    token::{get_transfer_fee_for_epoch, transfer_checked_with_remaining_accounts},
    transitions::liquidity::SwapCashPolicy,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseLeverageArgs {
    pub debt_asset: u8,
    pub min_amount_out: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct DelegatedCloseLeverageArgs {
    pub debt_asset: u8,
    pub min_amount_out: u64,
    /// Proportion of the current position to close. `10_000` preserves the
    /// existing full-close behavior.
    pub close_bps: u16,
    pub delegated: DelegatedCpiArgs,
}

#[derive(Clone, Copy)]
enum CloseMode {
    Owner,
    Delegate,
}

#[event_cpi]
#[derive(Accounts)]
pub struct CloseLeverage<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,

    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    /// CHECK: Receives closed account rent.
    #[account(mut, address = leverage_position.owner)]
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

    #[account(mut)]
    pub owner_debt_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub referral_partner: Option<Box<Account<'info, ReferralPartner>>>,

    #[account(mut)]
    pub referral_accrual: Option<Box<Account<'info, ReferralAccrual>>>,

    pub leverage_delegation: Option<Box<Account<'info, LeverageDelegation>>>,

    /// CHECK: Optional delegated program, validated in delegated mode.
    pub delegated_program: Option<UncheckedAccount<'info>>,

    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: Canonical Instructions sysvar for the launch split guard.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CloseLeverage<'info> {
    fn validate_common(&self, args: &CloseLeverageArgs, unix_timestamp: i64) -> Result<MarketAsset> {
        validate_leverage_market_pda(&self.market, self.market.key())?;
        validate_leverage_futarchy_pda(self.futarchy_authority.bump, self.futarchy_authority.key())?;
        self.market.assert_started_at(unix_timestamp)?;
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
        require_keys_eq!(
            self.owner_debt_account.mint,
            self.debt_mint.key(),
            ErrorCode::InvalidTokenAccount
        );
        self.leverage_position.require_open()?;
        self.leverage_position
            .assert_position(self.position_owner.key(), self.market.key(), debt_asset)?;
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
        Ok(debt_asset)
    }

    pub fn validate_at(&self, args: &CloseLeverageArgs, unix_timestamp: i64) -> Result<()> {
        self.validate_common(args, unix_timestamp)?;
        require_keys_eq!(
            self.authority.key(),
            self.position_owner.key(),
            ErrorCode::InvalidSigner
        );
        require_keys_eq!(
            self.owner_debt_account.owner,
            self.authority.key(),
            ErrorCode::InvalidTokenAccount
        );
        Ok(())
    }

    pub fn validate_delegated_at(&self, args: &DelegatedCloseLeverageArgs, unix_timestamp: i64) -> Result<()> {
        let debt_asset = self.validate_common(
            &CloseLeverageArgs {
                debt_asset: args.debt_asset,
                min_amount_out: args.min_amount_out,
            },
            unix_timestamp,
        )?;
        require!(
            args.close_bps > 0 && args.close_bps <= BPS_DENOMINATOR,
            ErrorCode::InvalidArgument
        );
        let delegation = self
            .leverage_delegation
            .as_ref()
            .ok_or(ErrorCode::InvalidLeverageDelegation)?;
        let delegated_program = self
            .delegated_program
            .as_ref()
            .ok_or(ErrorCode::InvalidLeverageDelegation)?;
        delegation.assert_delegation(
            self.position_owner.key(),
            self.market.key(),
            self.leverage_position.key(),
            debt_asset,
        )?;
        require_keys_eq!(
            delegation.delegated_program,
            delegated_program.key(),
            ErrorCode::InvalidLeverageDelegation
        );
        require!(
            delegation.approved_actions & LEVERAGE_DELEGATE_CLOSE == LEVERAGE_DELEGATE_CLOSE,
            ErrorCode::InvalidLeverageDelegation
        );
        Ok(())
    }

    pub fn handle_close(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: CloseLeverageArgs,
        current_slot: u64,
        current_epoch: u64,
        current_unix_timestamp: i64,
    ) -> Result<()> {
        Self::execute(
            ctx,
            args,
            None,
            CloseMode::Owner,
            BPS_DENOMINATOR,
            current_slot,
            current_epoch,
            current_unix_timestamp,
        )
    }

    pub fn handle_delegated_close(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: DelegatedCloseLeverageArgs,
        current_slot: u64,
        current_epoch: u64,
        current_unix_timestamp: i64,
    ) -> Result<()> {
        Self::execute(
            ctx,
            CloseLeverageArgs {
                debt_asset: args.debt_asset,
                min_amount_out: args.min_amount_out,
            },
            Some(args.delegated),
            CloseMode::Delegate,
            args.close_bps,
            current_slot,
            current_epoch,
            current_unix_timestamp,
        )
    }

    fn execute(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: CloseLeverageArgs,
        delegated: Option<DelegatedCpiArgs>,
        mode: CloseMode,
        close_bps: u16,
        current_slot: u64,
        current_epoch: u64,
        current_unix_timestamp: i64,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, ctx.remaining_accounts))?
        };
        let delegated = match mode {
            CloseMode::Owner => DelegatedCpiArgs::default(),
            CloseMode::Delegate => delegated.ok_or(ErrorCode::InvalidLeverageDelegation)?,
        };
        let owner_key = ctx.accounts.position_owner.key();
        let authority_key = ctx.accounts.authority.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let position_key = ctx.accounts.leverage_position.key();
        let expected_referral_partner = ctx.accounts.leverage_position.referral_partner;

        // Freeze debt indexes before deriving the exact proportional slice so
        // the delegate callback and committed lifecycle share one basis.
        ctx.accounts.market.accrue_interest_to_slot(current_slot)?;
        let close_slice = ctx
            .accounts
            .market
            .leverage_close_slice(&ctx.accounts.leverage_position, close_bps)?;
        let collateral_sold = close_slice.collateral_amount;
        let is_full_close = close_bps == BPS_DENOMINATOR;
        // Price the selected close before an optional delegated approval callback.
        let debt_amount = ctx
            .accounts
            .market
            .debt
            .isolated_repayment_for_max(debt_asset, close_slice.debt_shares, u64::MAX)?
            .cash_repaid;
        let expected_collateral_reserve_credit =
            leverage_collateral_credit(&ctx.accounts.collateral_mint, collateral_sold, current_epoch)?;
        ctx.accounts.market.prepare_amm_for_swap(current_slot)?;
        ctx.accounts.market.advance_one_amm_controller_target(current_slot)?;
        ctx.accounts.market.observe_current_risk(current_slot)?;
        let close_quote = ctx.accounts.market.quote_leverage_swap_at_time(
            collateral_asset,
            expected_collateral_reserve_credit,
            current_slot,
            current_unix_timestamp,
        )?;
        require_gte!(close_quote.amount_out, debt_amount, ErrorCode::InsufficientAmount);
        let expected_residual = close_quote
            .amount_out
            .checked_sub(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let expected_residual_net = expected_residual
            .checked_sub(get_transfer_fee_for_epoch(
                &ctx.accounts.debt_mint.to_account_info(),
                expected_residual,
                current_epoch,
            )?)
            .ok_or(ErrorCode::MarketMathOverflow)?;

        if matches!(mode, CloseMode::Delegate) {
            // Give the delegate the exact expected residual while protecting protocol accounts.
            let delegation = ctx
                .accounts
                .leverage_delegation
                .as_ref()
                .ok_or(ErrorCode::InvalidLeverageDelegation)?;
            let delegated_program = ctx
                .accounts
                .delegated_program
                .as_ref()
                .ok_or(ErrorCode::InvalidLeverageDelegation)?;
            let (before_accounts, _) = split_delegated_accounts(
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
                delegated.before_accounts_len,
            )?;
            let mut protected_accounts = vec![
                market_key,
                ctx.accounts.leverage_position.key(),
                delegation.key(),
                ctx.accounts.debt_reserve_vault.key(),
                ctx.accounts.collateral_reserve_vault.key(),
                ctx.accounts.debt_interest_vault.key(),
                ctx.accounts.leverage_collateral_vault.key(),
                ctx.accounts.owner_debt_account.key(),
            ];
            if let Some(partner) = ctx.accounts.referral_partner.as_ref() {
                protected_accounts.push(partner.key());
            }
            if let Some(accrual) = ctx.accounts.referral_accrual.as_ref() {
                protected_accounts.push(accrual.key());
            }
            ctx.accounts.market.exit(&crate::ID)?;
            ctx.accounts.leverage_position.exit(&crate::ID)?;
            invoke_delegated_approval_callback(
                delegated_program,
                delegated.before_ix_data.clone(),
                before_accounts,
                &protected_accounts,
                &[],
                LEVERAGE_DELEGATE_CLOSE,
                market_key,
                owner_key,
                position_key,
                delegation.key(),
                debt_asset,
                ctx.accounts.owner_debt_account.key(),
                debt_mint_key,
                collateral_sold,
                expected_residual_net,
            )?;
        }

        // Return collateral to the reserve and measure the swap's actual input.
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
        ctx.accounts.leverage_collateral_vault.reload()?;
        let collateral_reserve_credit = token_account_credit(
            collateral_reserve_balance_before,
            &ctx.accounts.collateral_reserve_vault,
        )?;
        require_eq!(
            collateral_reserve_credit,
            expected_collateral_reserve_credit,
            ErrorCode::BrokenInvariant
        );

        // Quote the credited collateral as the position's final debt repayment.
        let prepared_swap = prepare_leverage_swap(
            &mut ctx.accounts.market,
            SwapRequest {
                current_slot,
                current_unix_timestamp,
                asset_in: collateral_asset,
                reserve_credit: collateral_reserve_credit,
                protocol_fee_bps: ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            },
            SwapCashPolicy::Close {
                debt_asset,
                debt_shares: close_slice.debt_shares,
                debt_principal: close_slice.debt_principal,
            },
        )?;
        let swap = prepared_swap.swap;
        let interest_eligibility = prepared_swap.interest_eligibility;
        let swap_fee_credit = leverage_swap_fee_credit(&swap)?;

        // Commit the close and settle the resulting hLP exposure.
        let receipt = if is_full_close {
            ctx.accounts.market.close_leverage(
                &mut ctx.accounts.leverage_position,
                args.min_amount_out,
                prepared_swap,
                swap_fee_credit,
                ctx.accounts.futarchy_authority.revenue_share.swap_bps,
                ctx.accounts.futarchy_authority.protocol_auction_split,
                current_slot,
            )?
        } else {
            ctx.accounts.market.partial_close_leverage(
                &mut ctx.accounts.leverage_position,
                close_bps,
                args.min_amount_out,
                prepared_swap,
                swap_fee_credit,
                ctx.accounts.futarchy_authority.revenue_share.swap_bps,
                ctx.accounts.futarchy_authority.protocol_auction_split,
                current_slot,
                current_unix_timestamp,
            )?
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
        )?;
        // Inline hLP funding settlement may have credited this same vault.
        // Refresh before measuring the position-interest transfer so the
        // referral split cannot count the earlier hLP credit a second time.
        ctx.accounts.debt_interest_vault.reload()?;

        // Pay the owner's residual and route accrued interest.
        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let owner_balance_before = ctx.accounts.owner_debt_account.amount;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.owner_debt_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program,
            receipt.residual,
            ctx.accounts.debt_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(ctx.remaining_accounts),
        )?;
        ctx.accounts.owner_debt_account.reload()?;
        let residual_credit = token_account_credit(owner_balance_before, &ctx.accounts.owner_debt_account)?;
        require_gte!(residual_credit, args.min_amount_out, ErrorCode::SlippageExceeded);

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

        // Emit referral accrual before the final close event.
        if let Some(event) = referral_interest_accrued_event_at_slot(
            &referral_receipt,
            market_key,
            position_key,
            owner_key,
            authority_key,
            debt_mint_key,
            current_slot,
        )? {
            emit_cpi!(event);
        }

        let swap_event = LeverageSwapReceipt::new(
            receipt.swap,
            swap_fee_credit,
            ctx.accounts.market.base_side.reserves.live_reserve,
            ctx.accounts.market.quote_side.reserves.live_reserve,
        )?;
        if is_full_close {
            emit_cpi!(LeveragePositionClosed {
                market: market_key,
                position: position_key,
                owner: owner_key,
                debt_asset_mint: debt_mint_key,
                collateral_asset_mint: collateral_mint_key,
                debt_repaid: receipt.debt_repaid,
                interest_paid: receipt.interest_paid,
                collateral_sold: receipt.collateral_sold,
                closeout_value: receipt.closeout_value,
                residual: residual_credit,
                swap: swap_event,
                metadata: MarketEventMetadata::at_slot(authority_key, market_key, current_slot),
            });
        } else {
            emit_cpi!(LeveragePositionUpdated {
                market: market_key,
                position: position_key,
                owner: owner_key,
                debt_asset_mint: debt_mint_key,
                collateral_asset_mint: collateral_mint_key,
                borrowed_amount: 0,
                debt_delta: -i64::try_from(receipt.debt_reduced).map_err(|_| ErrorCode::Overflow)?,
                collateral_delta: -i64::try_from(receipt.collateral_sold).map_err(|_| ErrorCode::Overflow)?,
                debt_amount: receipt.remaining_debt_amount,
                debt_shares: receipt.remaining_debt_shares,
                collateral_amount: receipt.remaining_collateral_amount,
                closeout_value: receipt.remaining_closeout_value,
                owner_credit: residual_credit,
                swap: Some(swap_event),
                metadata: MarketEventMetadata::at_slot(authority_key, market_key, current_slot),
            });
        }

        if matches!(mode, CloseMode::Delegate) {
            // Notify the delegate only after settlement and protect the owner's payout.
            let delegation = ctx
                .accounts
                .leverage_delegation
                .as_ref()
                .ok_or(ErrorCode::InvalidLeverageDelegation)?;
            let delegated_program = ctx
                .accounts
                .delegated_program
                .as_ref()
                .ok_or(ErrorCode::InvalidLeverageDelegation)?;
            let (_, after_accounts) = split_delegated_accounts(
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
                delegated.before_accounts_len,
            )?;
            let mut protected_accounts = vec![
                market_key,
                ctx.accounts.leverage_position.key(),
                delegation.key(),
                ctx.accounts.debt_reserve_vault.key(),
                ctx.accounts.collateral_reserve_vault.key(),
                ctx.accounts.debt_interest_vault.key(),
                ctx.accounts.leverage_collateral_vault.key(),
                ctx.accounts.owner_debt_account.key(),
            ];
            if let Some(partner) = ctx.accounts.referral_partner.as_ref() {
                protected_accounts.push(partner.key());
            }
            if let Some(accrual) = ctx.accounts.referral_accrual.as_ref() {
                protected_accounts.push(accrual.key());
            }
            let writable_protected_accounts = [ctx.accounts.owner_debt_account.key()];
            ctx.accounts.market.exit(&crate::ID)?;
            ctx.accounts.leverage_position.exit(&crate::ID)?;
            invoke_delegated_approval_callback(
                delegated_program,
                delegated.after_ix_data,
                after_accounts,
                &protected_accounts,
                &writable_protected_accounts,
                LEVERAGE_DELEGATE_CLOSE_SETTLED,
                market_key,
                owner_key,
                position_key,
                delegation.key(),
                debt_asset,
                ctx.accounts.owner_debt_account.key(),
                debt_mint_key,
                collateral_sold,
                residual_credit,
            )?;
        }
        if is_full_close {
            ctx.accounts
                .leverage_position
                .close(ctx.accounts.position_owner.to_account_info())?;
        }
        Ok(())
    }
}

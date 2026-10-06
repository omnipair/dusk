use crate::transitions::amm::SwapRequest;
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use super::settlement::{
    invoke_delegated_approval_callback, leverage_collateral_credit, leverage_collateral_fee, leverage_swap_fee_credit,
    prepare_leverage_swap, record_leverage_interest, settle_inline_leverage_hlp, split_delegated_accounts,
    validate_leverage_futarchy_pda, validate_leverage_interest_account, validate_leverage_market_pda,
    validate_leverage_mints, validate_leverage_reserve_accounts, DelegatedCpiArgs, LEVERAGE_DELEGATE_CLOSE,
    LEVERAGE_DELEGATE_CLOSE_SETTLED,
};
use crate::{
    constants::*,
    errors::ErrorCode,
    events::{
        LeveragePositionClosed, LeveragePositionUpdated, LeverageSwapReceipt, MarketEventMetadata, SwapExecuted,
        SwapOrigin,
    },
    generate_market_seeds,
    instructions::{
        accounts::{require_reserve_custody, token_account_credit, token_program_for_mint, HlpSwapAccountLayout},
        enforce_launch_same_transaction_guard,
        referral::accounting::{referral_interest_accrued_event_at_slot, validate_referral_binding},
    },
    state::{
        FutarchyAuthority, LeverageDelegation, LeveragePosition, Market, MarketAsset, ReferralAccrual, ReferralPartner,
    },
    token::{get_transfer_fee_for_epoch, get_transfer_inverse_fee_for_epoch, transfer_checked_with_remaining_accounts},
    transitions::liquidity::SwapCashPolicy,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseLeverageArgs {
    pub debt_asset: u8,
    pub min_amount_out: u64,
}

/// Sell the owner's specified collateral amount to repay all debt, returning
/// unsold collateral. The swap must cover debt and the minimum collateral
/// payout or the close reverts. Any excess debt-token output is refunded.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseCollateralLeverageArgs {
    pub debt_asset: u8,
    pub collateral_in: u64,
    pub min_collateral_out: u64,
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

/// The order fee schedule is enforced by Dusk so a delegate cannot redirect
/// close proceeds or demand an unbounded payment from the owner's position.
pub const DELEGATED_CLOSE_PROTOCOL_FEE_BPS: u64 = 10;
pub const DELEGATED_CLOSE_EXECUTOR_INCENTIVE_BPS: u64 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DelegatedClosePayoutQuote {
    pub protocol_debit: u64,
    pub protocol_credit: u64,
    pub executor_debit: u64,
    pub executor_credit: u64,
    pub owner_debit: u64,
    pub owner_credit: u64,
}

/// Split only the realized residual. Protocol and executor payments are capped
/// by the available proceeds, so a thin but otherwise valid close can settle.
pub fn quote_delegated_close_payout(
    mint_info: &AccountInfo,
    residual: u64,
    execution_value: u64,
    incentive_basis: u64,
    epoch: u64,
) -> Result<DelegatedClosePayoutQuote> {
    let protocol_target = u64::try_from(
        (execution_value as u128 * DELEGATED_CLOSE_PROTOCOL_FEE_BPS as u128).div_ceil(BPS_DENOMINATOR as u128),
    )
    .map_err(|_| error!(ErrorCode::Overflow))?;
    let inverse_fee = if protocol_target == 0 {
        0
    } else {
        get_transfer_inverse_fee_for_epoch(mint_info, protocol_target, epoch)?
    };
    let protocol_debit = protocol_target.saturating_add(inverse_fee).min(residual);
    let protocol_credit = protocol_debit
        .checked_sub(get_transfer_fee_for_epoch(mint_info, protocol_debit, epoch)?)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let after_protocol = residual - protocol_debit;
    let executor_target = u64::try_from(
        (incentive_basis as u128 * DELEGATED_CLOSE_EXECUTOR_INCENTIVE_BPS as u128).div_ceil(BPS_DENOMINATOR as u128),
    )
    .map_err(|_| error!(ErrorCode::Overflow))?;
    let executor_debit = executor_target.min(after_protocol);
    let executor_credit = executor_debit
        .checked_sub(get_transfer_fee_for_epoch(mint_info, executor_debit, epoch)?)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    let owner_debit = after_protocol - executor_debit;
    let owner_credit = owner_debit
        .checked_sub(get_transfer_fee_for_epoch(mint_info, owner_debit, epoch)?)
        .ok_or(ErrorCode::MarketMathOverflow)?;
    Ok(DelegatedClosePayoutQuote {
        protocol_debit,
        protocol_credit,
        executor_debit,
        executor_credit,
        owner_debit,
        owner_credit,
    })
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
            leverage_position.owner.as_ref(),
            leverage_position.namespace_authority.as_ref(),
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

    /// Protocol treasury token account for delegated close order fees.
    #[account(mut)]
    pub delegate_fee_recipient: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

    /// Executor-owned token account for delegated close incentives.
    #[account(mut)]
    pub delegate_executor_account: Option<Box<InterfaceAccount<'info, TokenAccount>>>,

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
        require_keys_eq!(
            self.owner_debt_account.owner,
            self.position_owner.key(),
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
            self.leverage_position.open_curve_revision,
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
        let fee_recipient = self
            .delegate_fee_recipient
            .as_ref()
            .ok_or(ErrorCode::InvalidTokenAccount)?;
        let executor_account = self
            .delegate_executor_account
            .as_ref()
            .ok_or(ErrorCode::InvalidTokenAccount)?;
        require_keys_eq!(
            fee_recipient.owner,
            self.futarchy_authority.recipients.futarchy_treasury,
            ErrorCode::InvalidTokenAccount
        );
        require_keys_eq!(fee_recipient.mint, self.debt_mint.key(), ErrorCode::InvalidTokenAccount);
        require_keys_eq!(
            executor_account.owner,
            self.authority.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_eq!(
            executor_account.mint,
            self.debt_mint.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_neq!(
            fee_recipient.key(),
            executor_account.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_neq!(
            fee_recipient.key(),
            self.owner_debt_account.key(),
            ErrorCode::InvalidTokenAccount
        );
        require_keys_neq!(
            executor_account.key(),
            self.owner_debt_account.key(),
            ErrorCode::InvalidTokenAccount
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
            None,
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
            None,
        )
    }

    pub fn handle_collateral_close(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: CloseCollateralLeverageArgs,
        current_slot: u64,
        current_epoch: u64,
        current_unix_timestamp: i64,
    ) -> Result<()> {
        require_gt!(
            ctx.accounts.leverage_position.funded_collateral_amount,
            0,
            ErrorCode::InvalidArgument
        );
        Self::execute(
            ctx,
            CloseLeverageArgs {
                debt_asset: args.debt_asset,
                min_amount_out: 0,
            },
            None,
            CloseMode::Owner,
            BPS_DENOMINATOR,
            current_slot,
            current_epoch,
            current_unix_timestamp,
            Some((args.collateral_in, args.min_collateral_out)),
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
        native_close: Option<(u64, u64)>,
    ) -> Result<()> {
        // Native closes reserve the first remaining account for the owner's collateral payout.
        let (mut native_recipient, remaining_accounts) = if native_close.is_some() {
            let (account, rest) = ctx
                .remaining_accounts
                .split_first()
                .ok_or(ErrorCode::InvalidTokenAccount)?;
            let recipient = TokenAccount::try_deserialize(&mut account.try_borrow_data()?.as_ref())?;
            require_keys_eq!(
                *account.owner,
                *ctx.accounts.collateral_mint.to_account_info().owner,
                ErrorCode::InvalidTokenAccount
            );
            require_keys_eq!(
                recipient.owner,
                ctx.accounts.position_owner.key(),
                ErrorCode::InvalidTokenAccount
            );
            require_keys_eq!(
                recipient.mint,
                ctx.accounts.collateral_mint.key(),
                ErrorCode::InvalidTokenAccount
            );
            require!(account.is_writable, ErrorCode::InvalidTokenAccount);
            (Some(account.clone()), rest)
        } else {
            require_eq!(
                ctx.accounts.leverage_position.funded_collateral_amount,
                0,
                ErrorCode::InvalidArgument
            );
            (None, ctx.remaining_accounts)
        };
        let market_key = ctx.accounts.market.key();
        let h_lp_accounts = {
            let market: &Market = &ctx.accounts.market;
            HlpSwapAccountLayout::try_from((market, remaining_accounts))?
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
        crate::instructions::accounting::accrue_market_interest(
            &mut ctx.accounts.market,
            current_slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        let close_slice = ctx
            .accounts
            .market
            .leverage_close_slice(&ctx.accounts.leverage_position, close_bps)?;
        ctx.accounts.market.prepare_amm_for_swap(current_slot)?;
        ctx.accounts.market.advance_one_amm_controller_target(current_slot)?;
        ctx.accounts.market.observe_current_risk(current_slot)?;
        let collateral_sold = if let Some((amount, _)) = native_close {
            // Exact input is the gross custody debit. The quote below uses
            // the measured/fee-adjusted credit, including Token-2022 fees.
            amount
        } else {
            close_slice.collateral_amount
        };
        require!(
            collateral_sold > 0 && collateral_sold <= close_slice.collateral_amount,
            ErrorCode::InvalidArgument
        );
        let collateral_returned = close_slice.collateral_amount - collateral_sold;
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
        let expected_payout = if matches!(mode, CloseMode::Delegate) {
            let incentive_basis = if is_full_close {
                ctx.accounts.leverage_position.margin_amount
            } else {
                expected_residual_net
            };
            Some(quote_delegated_close_payout(
                &ctx.accounts.debt_mint.to_account_info(),
                expected_residual,
                close_quote.amount_out,
                incentive_basis,
                current_epoch,
            )?)
        } else {
            None
        };

        if matches!(mode, CloseMode::Delegate) {
            // Bind the delegate approval to the owner's exact net payout.
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
                h_lp_accounts.hook_accounts(remaining_accounts),
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
                ctx.accounts.position_owner.key(),
                ctx.accounts.authority.key(),
                ctx.accounts.futarchy_authority.key(),
                ctx.accounts.debt_mint.key(),
                ctx.accounts.collateral_mint.key(),
                ctx.accounts.instructions_sysvar.key(),
                ctx.accounts.token_program.key(),
                ctx.accounts.token_2022_program.key(),
            ];
            if let Some(account) = ctx.accounts.delegate_fee_recipient.as_ref() {
                protected_accounts.push(account.key());
            }
            if let Some(account) = ctx.accounts.delegate_executor_account.as_ref() {
                protected_accounts.push(account.key());
            }
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
                expected_payout
                    .ok_or(ErrorCode::InvalidLeverageDelegation)?
                    .owner_credit,
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
            collateral_token_program.clone(),
            collateral_sold,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(remaining_accounts),
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
        let request = SwapRequest {
            current_slot,
            current_unix_timestamp,
            asset_in: collateral_asset,
            reserve_credit: collateral_reserve_credit,
            protocol_fee_bps: ctx.accounts.futarchy_authority.revenue_share.swap_bps,
        };
        let policy = SwapCashPolicy::Close {
            debt_asset,
            debt_shares: close_slice.debt_shares,
            debt_principal: close_slice.debt_principal,
        };
        let prepared_swap = if native_close.is_some() {
            request.prepare_with_cash_policy_on_curve(&mut ctx.accounts.market, policy, true)?
        } else {
            prepare_leverage_swap(&mut ctx.accounts.market, request, policy)?
        };

        let swap = prepared_swap.leverage_quote();
        let interest_eligibility = prepared_swap.interest_eligibility;
        let swap_fee_credit = leverage_swap_fee_credit(&swap)?;

        // Commit the close and settle the resulting hLP exposure.
        let mut receipt = if is_full_close {
            ctx.accounts.market.close_leverage(
                &mut ctx.accounts.leverage_position,
                args.min_amount_out,
                prepared_swap,
                swap_fee_credit,
                ctx.accounts.futarchy_authority.revenue_share.swap_bps,
                ctx.accounts.futarchy_authority.protocol_auction_split,
                current_slot,
                leverage_collateral_fee(&ctx.accounts.collateral_mint, current_epoch)?,
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
                leverage_collateral_fee(&ctx.accounts.collateral_mint, current_epoch)?,
            )?
        };
        if native_close.is_some() {
            receipt.collateral_sold = collateral_sold;
        }
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
            remaining_accounts,
            h_lp_accounts,
            receipt.base_hlp_rebalance,
            receipt.quote_hlp_rebalance,
            interest_eligibility,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        // Inline hLP funding settlement may have credited this same vault.
        // Refresh before measuring the position-interest transfer so the
        // referral split cannot count the earlier hLP credit a second time.
        ctx.accounts.debt_interest_vault.reload()?;

        // Pay bounded order fees from the realized residual, then transfer the
        // remainder directly to an account controlled by the position owner.
        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        if let Some(payout) = expected_payout {
            require_eq!(receipt.residual, expected_residual, ErrorCode::BrokenInvariant);
            let fee_recipient = ctx
                .accounts
                .delegate_fee_recipient
                .as_deref_mut()
                .ok_or(ErrorCode::InvalidTokenAccount)?;
            let fee_before = fee_recipient.amount;
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.debt_reserve_vault.to_account_info(),
                fee_recipient.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                debt_token_program.clone(),
                payout.protocol_debit,
                ctx.accounts.debt_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
            )?;
            fee_recipient.reload()?;
            require_eq!(
                token_account_credit(fee_before, fee_recipient)?,
                payout.protocol_credit,
                ErrorCode::BrokenInvariant
            );
            let executor_account = ctx
                .accounts
                .delegate_executor_account
                .as_deref_mut()
                .ok_or(ErrorCode::InvalidTokenAccount)?;
            let executor_before = executor_account.amount;
            transfer_checked_with_remaining_accounts(
                ctx.accounts.market.to_account_info(),
                ctx.accounts.debt_reserve_vault.to_account_info(),
                executor_account.to_account_info(),
                ctx.accounts.debt_mint.to_account_info(),
                debt_token_program.clone(),
                payout.executor_debit,
                ctx.accounts.debt_mint.decimals,
                &[&generate_market_seeds!(ctx.accounts.market)[..]],
                h_lp_accounts.hook_accounts(ctx.remaining_accounts),
            )?;
            executor_account.reload()?;
            require_eq!(
                token_account_credit(executor_before, executor_account)?,
                payout.executor_credit,
                ErrorCode::BrokenInvariant
            );
        }
        let owner_balance_before = ctx.accounts.owner_debt_account.amount;
        transfer_checked_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.owner_debt_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program,
            expected_payout.map_or(receipt.residual, |payout| payout.owner_debit),
            ctx.accounts.debt_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            h_lp_accounts.hook_accounts(remaining_accounts),
        )?;
        ctx.accounts.owner_debt_account.reload()?;
        let residual_credit = token_account_credit(owner_balance_before, &ctx.accounts.owner_debt_account)?;
        if let Some(payout) = expected_payout {
            require_eq!(residual_credit, payout.owner_credit, ErrorCode::BrokenInvariant);
        }
        require_gte!(residual_credit, args.min_amount_out, ErrorCode::SlippageExceeded);

        let mut collateral_credit = 0;
        if let (Some(recipient), Some((_, minimum))) = (native_recipient.as_mut(), native_close) {
            let before = crate::instructions::accounts::token_account_info_amount(recipient)?;
            if collateral_returned > 0 {
                transfer_checked_with_remaining_accounts(
                    ctx.accounts.market.to_account_info(),
                    ctx.accounts.leverage_collateral_vault.to_account_info(),
                    recipient.to_account_info(),
                    ctx.accounts.collateral_mint.to_account_info(),
                    collateral_token_program,
                    collateral_returned,
                    ctx.accounts.collateral_mint.decimals,
                    &[&generate_market_seeds!(ctx.accounts.market)[..]],
                    h_lp_accounts.hook_accounts(remaining_accounts),
                )?;
            }
            collateral_credit = crate::instructions::accounts::token_account_info_amount(recipient)?
                .checked_sub(before)
                .ok_or(ErrorCode::MarketMathOverflow)?;
            require_gte!(collateral_credit, minimum, ErrorCode::SlippageExceeded);
        }

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
            h_lp_accounts.hook_accounts(remaining_accounts),
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
        emit_cpi!(SwapExecuted::from_leverage(
            market_key,
            owner_key,
            authority_key,
            position_key,
            SwapOrigin::LeverageClose,
            current_slot,
            swap_event,
            receipt.swap.start_price_nad,
            &ctx.accounts.market,
        )?);
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
                collateral_returned: collateral_credit,
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
                margin_terms: ctx.accounts.leverage_position.margin_terms,
                closeout_value: receipt.remaining_closeout_value,
                owner_credit: residual_credit,
                interest_paid: receipt.interest_paid,
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
                h_lp_accounts.hook_accounts(remaining_accounts),
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
                ctx.accounts.position_owner.key(),
                ctx.accounts.authority.key(),
                ctx.accounts.futarchy_authority.key(),
                ctx.accounts.debt_mint.key(),
                ctx.accounts.collateral_mint.key(),
                ctx.accounts.instructions_sysvar.key(),
                ctx.accounts.token_program.key(),
                ctx.accounts.token_2022_program.key(),
            ];
            if let Some(account) = ctx.accounts.delegate_fee_recipient.as_ref() {
                protected_accounts.push(account.key());
            }
            if let Some(account) = ctx.accounts.delegate_executor_account.as_ref() {
                protected_accounts.push(account.key());
            }
            if let Some(partner) = ctx.accounts.referral_partner.as_ref() {
                protected_accounts.push(partner.key());
            }
            if let Some(accrual) = ctx.accounts.referral_accrual.as_ref() {
                protected_accounts.push(accrual.key());
            }
            let writable_protected_accounts =
                [ctx.accounts.owner_debt_account.key(), ctx.accounts.position_owner.key()];
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

#[cfg(test)]
mod tests {
    include!("../../tests/instructions/leverage/close_leverage.rs");
}

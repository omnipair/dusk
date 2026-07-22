use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

use crate::{
    constants::*,
    errors::ErrorCode,
    events::{LeveragePositionClosed, MarketEventMetadata},
    generate_market_seeds,
    shared::token::{
        get_transfer_fee, transfer_from_vault_to_user_with_remaining_accounts,
        transfer_from_vault_to_vault_with_remaining_accounts,
    },
    state::{
        FutarchyAuthority, LeverageDelegation, LeveragePosition, Market, MarketAsset, ReferralAccrual, ReferralPartner,
    },
};

use super::common::{
    approved_for, invoke_delegated_approval_callback, move_leverage_swap_fee, record_leverage_interest,
    split_delegated_accounts, validate_leverage_fee_account, validate_leverage_interest_account,
    validate_leverage_mints, validate_leverage_reserve_accounts, DelegatedCpiArgs, LEVERAGE_DELEGATE_CLOSE,
    LEVERAGE_DELEGATE_CLOSE_SETTLED,
};
use crate::instructions::common::{token_account_credit, token_program_for_mint};
use crate::instructions::referral::common::{emit_referral_interest_accrued, validate_referral_binding};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseLeverageArgs {
    pub debt_asset: u8,
    pub min_amount_out: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct DelegatedCloseLeverageArgs {
    pub debt_asset: u8,
    pub min_amount_out: u64,
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

    #[account(
        seeds = [FUTARCHY_AUTHORITY_SEED_PREFIX],
        bump = futarchy_authority.bump
    )]
    pub futarchy_authority: Box<Account<'info, FutarchyAuthority>>,

    /// CHECK: Receives closed account rent.
    #[account(mut, address = leverage_position.owner)]
    pub position_owner: AccountInfo<'info>,

    #[account(
        mut,
        close = position_owner,
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
    pub collateral_fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
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
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CloseLeverage<'info> {
    fn validate_common(&self, args: &CloseLeverageArgs) -> Result<MarketAsset> {
        self.market.assert_started()?;
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        validate_leverage_mints(&self.market, debt_asset, &self.debt_mint, &self.collateral_mint)?;
        validate_leverage_reserve_accounts(
            &self.market,
            debt_asset,
            &self.debt_mint,
            &self.collateral_mint,
            &self.debt_reserve_vault,
            &self.collateral_reserve_vault,
        )?;
        validate_leverage_fee_account(
            &self.market,
            &self.collateral_mint,
            &self.collateral_fee_vault,
            debt_asset.opposite(),
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

    pub fn validate(&self, args: &CloseLeverageArgs) -> Result<()> {
        self.validate_common(args)?;
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

    pub fn validate_delegated(&self, args: &DelegatedCloseLeverageArgs) -> Result<()> {
        let debt_asset = self.validate_common(&CloseLeverageArgs {
            debt_asset: args.debt_asset,
            min_amount_out: args.min_amount_out,
        })?;
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
        approved_for(delegation.approved_actions, LEVERAGE_DELEGATE_CLOSE)?;
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(CloseLeverageArgs);

    pub fn update_and_validate_delegated(&mut self, args: &DelegatedCloseLeverageArgs) -> Result<()> {
        self.update()?;
        self.validate_delegated(args)
    }

    pub fn handle_close(ctx: Context<'_, '_, '_, 'info, Self>, args: CloseLeverageArgs) -> Result<()> {
        Self::execute(ctx, args, None, CloseMode::Owner)
    }

    pub fn handle_delegated_close(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: DelegatedCloseLeverageArgs,
    ) -> Result<()> {
        Self::execute(
            ctx,
            CloseLeverageArgs {
                debt_asset: args.debt_asset,
                min_amount_out: args.min_amount_out,
            },
            Some(args.delegated),
            CloseMode::Delegate,
        )
    }

    fn execute(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: CloseLeverageArgs,
        delegated: Option<DelegatedCpiArgs>,
        mode: CloseMode,
    ) -> Result<()> {
        let delegated = match mode {
            CloseMode::Owner => DelegatedCpiArgs::default(),
            CloseMode::Delegate => delegated.ok_or(ErrorCode::InvalidLeverageDelegation)?,
        };
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.position_owner.key();
        let authority_key = ctx.accounts.authority.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let position_key = ctx.accounts.leverage_position.key();
        let expected_referral_partner = ctx.accounts.leverage_position.referral_partner;
        let collateral_sold = ctx.accounts.leverage_position.collateral_amount;
        let debt_amount = ctx.accounts.leverage_position.debt_amount(&ctx.accounts.market.debt)?;
        let close_quote = ctx
            .accounts
            .market
            .quote_leverage_swap(collateral_asset, collateral_sold)?;
        require_gte!(close_quote.amount_out, debt_amount, ErrorCode::InsufficientAmount);
        let expected_residual = close_quote
            .amount_out
            .checked_sub(debt_amount)
            .ok_or(ErrorCode::MarketMathOverflow)?;
        let expected_residual_net = transfer_net_amount(&ctx.accounts.debt_mint.to_account_info(), expected_residual)?;

        if matches!(mode, CloseMode::Delegate) {
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
            let (before_accounts, _) = split_delegated_accounts(ctx.remaining_accounts, delegated.before_accounts_len)?;
            let mut protected_accounts = vec![
                ctx.accounts.market.key(),
                ctx.accounts.leverage_position.key(),
                delegation.key(),
                ctx.accounts.debt_reserve_vault.key(),
                ctx.accounts.collateral_reserve_vault.key(),
                ctx.accounts.collateral_fee_vault.key(),
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
                expected_residual_net,
            )?;
        }

        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        transfer_from_vault_to_vault_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            collateral_sold,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        ctx.accounts.leverage_collateral_vault.reload()?;

        let swap = ctx
            .accounts
            .market
            .quote_leverage_swap(collateral_asset, collateral_sold)?;
        move_leverage_swap_fee(
            &ctx.accounts.market,
            &ctx.accounts.collateral_mint,
            &mut ctx.accounts.collateral_reserve_vault,
            &mut ctx.accounts.collateral_fee_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            swap.fee_credit,
            ctx.remaining_accounts,
        )?;

        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let receipt = ctx.accounts.market.close_leverage(
            &mut ctx.accounts.leverage_position,
            args.min_amount_out,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
        )?;

        let debt_token_program = token_program_for_mint(
            &ctx.accounts.debt_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let owner_balance_before = ctx.accounts.owner_debt_account.amount;
        transfer_from_vault_to_user_with_remaining_accounts(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.debt_reserve_vault.to_account_info(),
            ctx.accounts.owner_debt_account.to_account_info(),
            ctx.accounts.debt_mint.to_account_info(),
            debt_token_program,
            receipt.residual,
            ctx.accounts.debt_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
            ctx.remaining_accounts,
        )?;
        ctx.accounts.owner_debt_account.reload()?;
        let residual_credit = token_account_credit(owner_balance_before, &ctx.accounts.owner_debt_account)?;
        require_gte!(residual_credit, args.min_amount_out, ErrorCode::SlippageExceeded);

        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let referral_receipt = record_leverage_interest(
            &mut ctx.accounts.market,
            debt_asset,
            &ctx.accounts.debt_mint,
            &mut ctx.accounts.debt_reserve_vault,
            &mut ctx.accounts.debt_interest_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            manager_fee_bps,
            &ctx.accounts.futarchy_authority,
            expected_referral_partner,
            ctx.accounts.leverage_position.referral_interest_share_bps,
            ctx.accounts.referral_partner.as_deref(),
            ctx.accounts.referral_accrual.as_deref_mut(),
            receipt.interest_paid,
            ctx.remaining_accounts,
        )?;

        emit_referral_interest_accrued(
            &referral_receipt,
            market_key,
            position_key,
            owner_key,
            authority_key,
            debt_mint_key,
        )?;

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
            metadata: MarketEventMetadata::new(authority_key, market_key)?,
        });

        if matches!(mode, CloseMode::Delegate) {
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
            let (_, after_accounts) = split_delegated_accounts(ctx.remaining_accounts, delegated.before_accounts_len)?;
            let mut protected_accounts = vec![
                ctx.accounts.market.key(),
                ctx.accounts.leverage_position.key(),
                delegation.key(),
                ctx.accounts.debt_reserve_vault.key(),
                ctx.accounts.collateral_reserve_vault.key(),
                ctx.accounts.collateral_fee_vault.key(),
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
                residual_credit,
            )?;
        }
        Ok(())
    }
}

fn transfer_net_amount(mint: &AccountInfo, gross_amount: u64) -> Result<u64> {
    let fee = get_transfer_fee(mint, gross_amount)?;
    gross_amount
        .checked_sub(fee)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

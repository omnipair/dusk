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
    instructions::common::{token_account_credit, token_program_for_mint, validate_owner_asset_account},
    shared::token::{get_transfer_fee, transfer_from_vault_to_user, transfer_from_vault_to_vault},
    state::{FutarchyAuthority, LeverageDelegation, LeverageMarginMode, LeveragePosition, Market, MarketAsset},
};

use super::common::{
    approved_for, invoke_delegated_approval_callback, leverage_transfer_amount_for_credit, move_leverage_swap_fee,
    record_leverage_interest, split_delegated_accounts, validate_leverage_fee_account,
    validate_leverage_interest_account, validate_leverage_mints, validate_leverage_reserve_accounts, DelegatedCpiArgs,
    LEVERAGE_DELEGATE_CLOSE, LEVERAGE_DELEGATE_CLOSE_SETTLED,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CloseCollateralMarginLeverageArgs {
    pub debt_asset: u8,
    pub max_collateral_in: u64,
    pub min_residual_out: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct DelegatedCloseCollateralMarginLeverageArgs {
    pub debt_asset: u8,
    pub max_collateral_in: u64,
    pub min_residual_out: u64,
    pub delegated: DelegatedCpiArgs,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: CloseCollateralMarginLeverageArgs)]
pub struct CloseCollateralMarginLeverage<'info> {
    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_mint.as_ref(),
            market.quote_mint.as_ref(),
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

    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        close = owner,
        seeds = [
            LEVERAGE_POSITION_SEED_PREFIX,
            market.key().as_ref(),
            leverage_position.position_id.as_ref(),
        ],
        bump = leverage_position.bump,
        constraint = leverage_position.owner == owner.key() @ ErrorCode::InvalidLeveragePosition,
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
    pub owner_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

#[event_cpi]
#[derive(Accounts)]
#[instruction(args: DelegatedCloseCollateralMarginLeverageArgs)]
pub struct DelegatedCloseCollateralMarginLeverage<'info> {
    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_mint.as_ref(),
            market.quote_mint.as_ref(),
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

    /// CHECK: Bound to the position owner and receives closed account rent.
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

    #[account(
        mut,
        constraint = recipient_collateral_account.mint == collateral_mint.key() @ ErrorCode::InvalidTokenAccount,
    )]
    pub recipient_collateral_account: Box<InterfaceAccount<'info, TokenAccount>>,

    pub leverage_delegation: Box<Account<'info, LeverageDelegation>>,

    /// CHECK: Validated against the delegation and required to be executable by the callback helper.
    pub delegated_program: UncheckedAccount<'info>,

    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}

impl<'info> CloseCollateralMarginLeverage<'info> {
    pub fn validate(&self, args: &CloseCollateralMarginLeverageArgs) -> Result<()> {
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
        validate_owner_asset_account(self.owner.key(), &self.collateral_mint, &self.owner_collateral_account)?;
        self.leverage_position.require_open()?;
        self.leverage_position
            .assert_position(self.owner.key(), self.market.key(), debt_asset)?;
        self.leverage_position
            .require_margin_mode(LeverageMarginMode::Collateral)?;

        let debt_amount = self.leverage_position.debt_amount(&self.market.debt)?;
        let swap = self
            .market
            .quote_leverage_swap_exact_output(debt_asset.opposite(), debt_amount)?;
        let collateral_debit = leverage_transfer_amount_for_credit(&self.collateral_mint, swap.amount_in)?;
        require_gte!(args.max_collateral_in, collateral_debit, ErrorCode::SlippageExceeded);
        require_gte!(
            self.leverage_position.collateral_amount,
            collateral_debit,
            ErrorCode::InsufficientAmount
        );
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(CloseCollateralMarginLeverageArgs);

    pub fn handle_close(ctx: Context<'_, '_, '_, 'info, Self>, args: CloseCollateralMarginLeverageArgs) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.owner.key();
        let position_key = ctx.accounts.leverage_position.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let debt_amount = ctx.accounts.leverage_position.debt_amount(&ctx.accounts.market.debt)?;
        let swap = ctx
            .accounts
            .market
            .quote_leverage_swap_exact_output(collateral_asset, debt_amount)?;
        let collateral_debit = leverage_transfer_amount_for_credit(&ctx.accounts.collateral_mint, swap.amount_in)?;
        require_gte!(args.max_collateral_in, collateral_debit, ErrorCode::SlippageExceeded);
        require_gte!(
            ctx.accounts.leverage_position.collateral_amount,
            collateral_debit,
            ErrorCode::InsufficientAmount
        );

        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let reserve_balance_before = ctx.accounts.collateral_reserve_vault.amount;
        transfer_from_vault_to_vault(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program.clone(),
            collateral_debit,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        ctx.accounts.leverage_collateral_vault.reload()?;
        let reserve_credit = token_account_credit(reserve_balance_before, &ctx.accounts.collateral_reserve_vault)?;
        require_eq!(reserve_credit, swap.amount_in, ErrorCode::UnexpectedTokenTransferAmount);

        move_leverage_swap_fee(
            &ctx.accounts.market,
            &ctx.accounts.collateral_mint,
            &mut ctx.accounts.collateral_reserve_vault,
            &mut ctx.accounts.collateral_fee_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            swap.fee_credit,
        )?;

        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let receipt = ctx.accounts.market.close_collateral_margin_leverage(
            &mut ctx.accounts.leverage_position,
            collateral_debit,
            args.max_collateral_in,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
        )?;

        let owner_balance_before = ctx.accounts.owner_collateral_account.amount;
        transfer_from_vault_to_user(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.owner_collateral_account.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            receipt.residual,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.owner_collateral_account.reload()?;
        ctx.accounts.leverage_collateral_vault.reload()?;
        let residual_credit = token_account_credit(owner_balance_before, &ctx.accounts.owner_collateral_account)?;
        require_gte!(residual_credit, args.min_residual_out, ErrorCode::SlippageExceeded);

        record_leverage_interest(
            &mut ctx.accounts.market,
            debt_asset,
            &ctx.accounts.debt_mint,
            &mut ctx.accounts.debt_reserve_vault,
            &mut ctx.accounts.debt_interest_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.interest_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
            receipt.interest_paid,
        )?;

        emit_cpi!(LeveragePositionClosed {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            margin_mode: LeverageMarginMode::Collateral.code(),
            margin_asset_mint: collateral_mint_key,
            settlement_asset_mint: collateral_mint_key,
            debt_repaid: receipt.debt_repaid,
            interest_paid: receipt.interest_paid,
            collateral_sold: receipt.collateral_sold,
            closeout_value: receipt.closeout_value,
            residual: residual_credit,
            metadata: MarketEventMetadata::new(owner_key, market_key)?,
        });
        Ok(())
    }
}

impl<'info> DelegatedCloseCollateralMarginLeverage<'info> {
    pub fn validate(&self, args: &DelegatedCloseCollateralMarginLeverageArgs) -> Result<()> {
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
        self.leverage_position.require_open()?;
        self.leverage_position
            .assert_position(self.position_owner.key(), self.market.key(), debt_asset)?;
        self.leverage_position
            .require_margin_mode(LeverageMarginMode::Collateral)?;
        self.leverage_delegation.assert_delegation(
            self.position_owner.key(),
            self.market.key(),
            self.leverage_position.key(),
            debt_asset,
        )?;
        require_keys_eq!(
            self.leverage_delegation.delegated_program,
            self.delegated_program.key(),
            ErrorCode::InvalidLeverageDelegation
        );
        approved_for(self.leverage_delegation.approved_actions, LEVERAGE_DELEGATE_CLOSE)?;

        let debt_amount = self.leverage_position.debt_amount(&self.market.debt)?;
        let swap = self
            .market
            .quote_leverage_swap_exact_output(debt_asset.opposite(), debt_amount)?;
        let collateral_debit = leverage_transfer_amount_for_credit(&self.collateral_mint, swap.amount_in)?;
        require_gte!(args.max_collateral_in, collateral_debit, ErrorCode::SlippageExceeded);
        let residual = self
            .leverage_position
            .collateral_amount
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let expected_residual_net = transfer_net_amount(&self.collateral_mint.to_account_info(), residual)?;
        require_gte!(
            expected_residual_net,
            args.min_residual_out,
            ErrorCode::SlippageExceeded
        );
        Ok(())
    }

    crate::instructions::common::market_update_and_validate!(DelegatedCloseCollateralMarginLeverageArgs);

    pub fn handle_delegated_close(
        ctx: Context<'_, '_, '_, 'info, Self>,
        args: DelegatedCloseCollateralMarginLeverageArgs,
    ) -> Result<()> {
        let market_key = ctx.accounts.market.key();
        let owner_key = ctx.accounts.position_owner.key();
        let authority_key = ctx.accounts.authority.key();
        let position_key = ctx.accounts.leverage_position.key();
        let delegation_key = ctx.accounts.leverage_delegation.key();
        let debt_asset = MarketAsset::try_from_code(args.debt_asset)?;
        let collateral_asset = debt_asset.opposite();
        let debt_mint_key = ctx.accounts.debt_mint.key();
        let collateral_mint_key = ctx.accounts.collateral_mint.key();
        let recipient_key = ctx.accounts.recipient_collateral_account.key();
        let debt_amount = ctx.accounts.leverage_position.debt_amount(&ctx.accounts.market.debt)?;
        let swap = ctx
            .accounts
            .market
            .quote_leverage_swap_exact_output(collateral_asset, debt_amount)?;
        let collateral_debit = leverage_transfer_amount_for_credit(&ctx.accounts.collateral_mint, swap.amount_in)?;
        require_gte!(args.max_collateral_in, collateral_debit, ErrorCode::SlippageExceeded);
        let expected_residual = ctx
            .accounts
            .leverage_position
            .collateral_amount
            .checked_sub(collateral_debit)
            .ok_or(ErrorCode::InsufficientAmount)?;
        let expected_residual_net =
            transfer_net_amount(&ctx.accounts.collateral_mint.to_account_info(), expected_residual)?;
        require_gte!(
            expected_residual_net,
            args.min_residual_out,
            ErrorCode::SlippageExceeded
        );

        let (before_accounts, _) =
            split_delegated_accounts(ctx.remaining_accounts, args.delegated.before_accounts_len)?;
        let protected_accounts = [
            market_key,
            position_key,
            delegation_key,
            ctx.accounts.debt_reserve_vault.key(),
            ctx.accounts.collateral_reserve_vault.key(),
            ctx.accounts.leverage_collateral_vault.key(),
            recipient_key,
        ];
        ctx.accounts.market.exit(&crate::ID)?;
        ctx.accounts.leverage_position.exit(&crate::ID)?;
        invoke_delegated_approval_callback(
            &ctx.accounts.delegated_program,
            args.delegated.before_ix_data.clone(),
            before_accounts,
            &protected_accounts,
            &[],
            LEVERAGE_DELEGATE_CLOSE,
            market_key,
            owner_key,
            position_key,
            delegation_key,
            debt_asset,
            recipient_key,
            collateral_mint_key,
            expected_residual_net,
        )?;

        let collateral_token_program = token_program_for_mint(
            &ctx.accounts.collateral_mint,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
        )?;
        let reserve_balance_before = ctx.accounts.collateral_reserve_vault.amount;
        transfer_from_vault_to_vault(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.collateral_reserve_vault.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program.clone(),
            collateral_debit,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.collateral_reserve_vault.reload()?;
        ctx.accounts.leverage_collateral_vault.reload()?;
        let reserve_credit = token_account_credit(reserve_balance_before, &ctx.accounts.collateral_reserve_vault)?;
        require_eq!(reserve_credit, swap.amount_in, ErrorCode::UnexpectedTokenTransferAmount);

        move_leverage_swap_fee(
            &ctx.accounts.market,
            &ctx.accounts.collateral_mint,
            &mut ctx.accounts.collateral_reserve_vault,
            &mut ctx.accounts.collateral_fee_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            swap.fee_credit,
        )?;

        let manager_fee_bps = ctx.accounts.market.config.manager_fee_bps;
        let receipt = ctx.accounts.market.close_collateral_margin_leverage(
            &mut ctx.accounts.leverage_position,
            collateral_debit,
            args.max_collateral_in,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.swap_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
        )?;

        let recipient_balance_before = ctx.accounts.recipient_collateral_account.amount;
        transfer_from_vault_to_user(
            ctx.accounts.market.to_account_info(),
            ctx.accounts.leverage_collateral_vault.to_account_info(),
            ctx.accounts.recipient_collateral_account.to_account_info(),
            ctx.accounts.collateral_mint.to_account_info(),
            collateral_token_program,
            receipt.residual,
            ctx.accounts.collateral_mint.decimals,
            &[&generate_market_seeds!(ctx.accounts.market)[..]],
        )?;
        ctx.accounts.recipient_collateral_account.reload()?;
        ctx.accounts.leverage_collateral_vault.reload()?;
        let residual_credit =
            token_account_credit(recipient_balance_before, &ctx.accounts.recipient_collateral_account)?;
        require_eq!(
            residual_credit,
            expected_residual_net,
            ErrorCode::UnexpectedTokenTransferAmount
        );
        require_gte!(residual_credit, args.min_residual_out, ErrorCode::SlippageExceeded);

        record_leverage_interest(
            &mut ctx.accounts.market,
            debt_asset,
            &ctx.accounts.debt_mint,
            &mut ctx.accounts.debt_reserve_vault,
            &mut ctx.accounts.debt_interest_vault,
            &ctx.accounts.token_program,
            &ctx.accounts.token_2022_program,
            manager_fee_bps,
            ctx.accounts.futarchy_authority.revenue_share.interest_bps,
            ctx.accounts.futarchy_authority.protocol_auction_split,
            receipt.interest_paid,
        )?;

        emit_cpi!(LeveragePositionClosed {
            market: market_key,
            position: position_key,
            owner: owner_key,
            debt_asset_mint: debt_mint_key,
            collateral_asset_mint: collateral_mint_key,
            margin_mode: LeverageMarginMode::Collateral.code(),
            margin_asset_mint: collateral_mint_key,
            settlement_asset_mint: collateral_mint_key,
            debt_repaid: receipt.debt_repaid,
            interest_paid: receipt.interest_paid,
            collateral_sold: receipt.collateral_sold,
            closeout_value: receipt.closeout_value,
            residual: residual_credit,
            metadata: MarketEventMetadata::new(authority_key, market_key)?,
        });

        let (_, after_accounts) = split_delegated_accounts(ctx.remaining_accounts, args.delegated.before_accounts_len)?;
        let writable_protected_accounts = [recipient_key];
        ctx.accounts.market.exit(&crate::ID)?;
        ctx.accounts.leverage_position.exit(&crate::ID)?;
        invoke_delegated_approval_callback(
            &ctx.accounts.delegated_program,
            args.delegated.after_ix_data,
            after_accounts,
            &protected_accounts,
            &writable_protected_accounts,
            LEVERAGE_DELEGATE_CLOSE_SETTLED,
            market_key,
            owner_key,
            position_key,
            delegation_key,
            debt_asset,
            recipient_key,
            collateral_mint_key,
            residual_credit,
        )?;
        Ok(())
    }
}

fn transfer_net_amount(mint: &AccountInfo, gross_amount: u64) -> Result<u64> {
    let fee = get_transfer_fee(mint, gross_amount)?;
    gross_amount
        .checked_sub(fee)
        .ok_or(ErrorCode::MarketMathOverflow.into())
}

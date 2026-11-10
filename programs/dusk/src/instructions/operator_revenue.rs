use crate::{
    account::get_size_with_discriminator,
    constants::*,
    errors::ErrorCode,
    generate_market_seeds,
    instructions::accounts::{
        require_supported_asset_mint, token_account_credit, token_program_for_mint, validate_interest_accounts,
    },
    state::*,
    token::transfer_checked_with_remaining_accounts,
};
use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::get_associated_token_address_with_program_id,
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

#[derive(Accounts)]
pub struct InitializeOperatorRevenue<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion, seeds=[MARKET_V2_SEED_PREFIX,market.base_side.asset_mint.as_ref(),market.quote_side.asset_mint.as_ref(),market.params_hash.as_ref()],bump=market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(init,payer=payer,space=get_size_with_discriminator::<OperatorRevenueAccount>(),
        seeds=[OPERATOR_REVENUE_SEED,market.key().as_ref(),&market.operator.revenue_epoch.to_le_bytes()],bump)]
    pub revenue: Box<Account<'info, OperatorRevenueAccount>>,
    pub system_program: Program<'info, System>,
}
impl InitializeOperatorRevenue<'_> {
    pub fn handle(ctx: Context<Self>) -> Result<()> {
        ctx.accounts.market.assert_current_version()?;
        let revenue = &mut ctx.accounts.revenue;
        revenue.market = ctx.accounts.market.key();
        revenue.recipient = ctx.accounts.market.operator.revenue_recipient;
        revenue.revenue_epoch = ctx.accounts.market.operator.revenue_epoch;
        revenue.claimable = [0; 4];
        revenue.sealed = false;
        revenue.bump = ctx.bumps.revenue;
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ClaimOperatorRevenueArgs {
    pub revenue_epoch: u64,
    pub max_debit: u64,
    pub minimum_credit: u64,
}
#[event]
pub struct OperatorRevenueClaimed {
    pub market: Pubkey,
    pub recipient: Pubkey,
    pub revenue_epoch: u64,
    pub asset_mint: Pubkey,
    pub gross_debit: u64,
    pub net_credit: u64,
}
#[event_cpi]
#[derive(Accounts)]
pub struct ClaimOperatorRevenue<'info> {
    #[account(mut, constraint = market.assert_current_version().is_ok() @ ErrorCode::InvalidVersion,seeds=[MARKET_V2_SEED_PREFIX,market.base_side.asset_mint.as_ref(),market.quote_side.asset_mint.as_ref(),market.params_hash.as_ref()],bump=market.bump)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut)]
    pub revenue: Option<Box<Account<'info, OperatorRevenueAccount>>>,
    /// CHECK: Entitlement recipient is checked against the current role or a sealed archive.
    pub recipient: UncheckedAccount<'info>,
    pub asset_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut)]
    pub interest_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub recipient_asset_account: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub token_2022_program: Program<'info, Token2022>,
}
impl ClaimOperatorRevenue<'_> {
    pub fn handle<'info>(
        ctx: Context<'_, '_, '_, 'info, ClaimOperatorRevenue<'info>>,
        args: ClaimOperatorRevenueArgs,
    ) -> Result<()> {
        let a = &mut *ctx.accounts;
        a.market.assert_current_version()?;
        require!(args.max_debit > 0, ErrorCode::AmountZero);
        let asset = a.market.asset_for_mint(a.asset_mint.key())?;
        require_supported_asset_mint(&a.asset_mint)?;
        validate_interest_accounts(&a.market, &a.asset_mint, &a.interest_vault)?;
        require_keys_eq!(a.interest_vault.owner, a.market.key(), ErrorCode::InvalidVault);
        let current = args.revenue_epoch == a.market.operator.revenue_epoch;
        let recipient = if current {
            require!(a.revenue.is_none(), ErrorCode::InvalidArgument);
            a.market.operator.revenue_recipient
        } else {
            require!(
                args.revenue_epoch < a.market.operator.revenue_epoch,
                ErrorCode::InvalidArgument
            );
            let archive = a.revenue.as_ref().ok_or(ErrorCode::InvalidArgument)?;
            archive.validate_identity(a.market.key(), args.revenue_epoch, archive.key())?;
            archive.recipient
        };
        require_keys_eq!(a.recipient.key(), recipient, ErrorCode::InvalidRecipient);
        let token_program = token_program_for_mint(&a.asset_mint, &a.token_program, &a.token_2022_program)?;
        let ata = get_associated_token_address_with_program_id(&recipient, &a.asset_mint.key(), token_program.key);
        require_keys_eq!(a.recipient_asset_account.key(), ata, ErrorCode::InvalidTokenAccount);
        require_keys_eq!(a.recipient_asset_account.owner, recipient, ErrorCode::InvalidRecipient);
        require_keys_eq!(
            a.recipient_asset_account.mint,
            a.asset_mint.key(),
            ErrorCode::InvalidMint
        );
        require_keys_eq!(
            *a.recipient_asset_account.to_account_info().owner,
            *token_program.key,
            ErrorCode::InvalidTokenProgram
        );
        let mut total = 0_u64;
        for i in [asset.code() as usize, asset.code() as usize + 2] {
            let amount = if current {
                let revenue = a.market.income.get_mut()?.revenue[i].as_mut();
                let amount = revenue.operator_claimable.min(args.max_debit - total);
                revenue.operator_claimable -= amount;
                amount
            } else {
                let claim = &mut a.revenue.as_mut().ok_or(ErrorCode::InvalidArgument)?.claimable[i];
                let amount = (*claim).min(args.max_debit - total);
                *claim -= amount;
                amount
            };
            total = total.checked_add(amount).ok_or(ErrorCode::MarketMathOverflow)?;
        }
        require!(total > 0, ErrorCode::AmountZero);
        let fees = &mut a.market.side_mut(asset).fees;
        require_gte!(
            a.interest_vault.amount,
            fees.interest_vault_balance,
            ErrorCode::UnbackedFeeLiability
        );
        fees.revenue_interest_liability = fees
            .revenue_interest_liability
            .checked_sub(total)
            .ok_or(ErrorCode::UnbackedFeeLiability)?;
        fees.interest_vault_balance = fees
            .interest_vault_balance
            .checked_sub(total)
            .ok_or(ErrorCode::UnbackedFeeLiability)?;
        fees.assert_backed()?;
        let before = a.recipient_asset_account.amount;
        transfer_checked_with_remaining_accounts(
            a.market.to_account_info(),
            a.interest_vault.to_account_info(),
            a.recipient_asset_account.to_account_info(),
            a.asset_mint.to_account_info(),
            token_program,
            total,
            a.asset_mint.decimals,
            &[&generate_market_seeds!(a.market)[..]],
            ctx.remaining_accounts,
        )?;
        a.recipient_asset_account.reload()?;
        let credit = token_account_credit(before, &a.recipient_asset_account)?;
        require_gte!(credit, args.minimum_credit, ErrorCode::InsufficientOutputAmount);
        let event = OperatorRevenueClaimed {
            market: a.market.key(),
            recipient,
            revenue_epoch: args.revenue_epoch,
            asset_mint: a.asset_mint.key(),
            gross_debit: total,
            net_credit: credit,
        };
        emit_cpi!(event);
        Ok(())
    }
}

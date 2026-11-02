use super::{carry_forward_governance_yield, checkpoint_supporter_yield, validate_market_pda};
use crate::{
    account::get_size_with_discriminator,
    constants::*,
    errors::ErrorCode,
    events::GovernanceBalanceUpdated,
    generate_market_seeds,
    instructions::accounts::{validate_lp_mint, validate_owner_lp_account},
    state::{GovernanceBalance, GovernanceTokenKind, Market, VirtualYieldLedger, YieldAccount, YieldTokenKind},
    token::{token_burn, token_mint_to},
};
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub enum GovernanceBalanceAction {
    Deposit {
        amount: u64,
    },
    Withdraw {
        amount: u64,
    },
    /// Credit earned yield to the owner's ordinary YieldAccounts for harvesting.
    ClaimYield,
}

#[event_cpi]
#[derive(Accounts)]
pub struct ManageGovernanceBalance<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [
            MARKET_V2_SEED_PREFIX,
            market.base_side.asset_mint.as_ref(),
            market.quote_side.asset_mint.as_ref(),
            market.params_hash.as_ref(),
        ],
        bump = market.bump,
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        init_if_needed, payer = owner,
        space = get_size_with_discriminator::<GovernanceBalance>(),
        seeds = [GOVERNANCE_BALANCE_SEED_PREFIX, market.key().as_ref(), owner.key().as_ref(), lp_mint.key().as_ref()], bump,
    )]
    pub governance_balance: Box<Account<'info, GovernanceBalance>>,

    #[account(mut)]
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut)]
    pub owner_lp_account: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            lp_mint.key().as_ref(),
            market.base_side.asset_mint.as_ref(),
            &[base_yield_account.token_kind],
        ],
        bump = base_yield_account.bump,
    )]
    pub base_yield_account: Box<Account<'info, YieldAccount>>,

    #[account(
        mut,
        seeds = [
            YIELD_ACCOUNT_SEED_PREFIX,
            market.key().as_ref(),
            owner.key().as_ref(),
            lp_mint.key().as_ref(),
            market.quote_side.asset_mint.as_ref(),
            &[quote_yield_account.token_kind],
        ],
        bump = quote_yield_account.bump,
    )]
    pub quote_yield_account: Box<Account<'info, YieldAccount>>,

    pub token_2022_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

impl<'info> ManageGovernanceBalance<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, action: GovernanceBalanceAction) -> Result<()> {
        validate_market_pda(&ctx.accounts.market, ctx.accounts.market.key())?;
        validate_lp_mint(
            &ctx.accounts.lp_mint,
            ctx.accounts.market.key(),
            ctx.accounts.market.base_side.asset_decimals,
        )?;
        let token_kind = if ctx.accounts.lp_mint.key() == ctx.accounts.market.ylp_mint {
            GovernanceTokenKind::Senior
        } else {
            require_keys_eq!(
                ctx.accounts.lp_mint.key(),
                ctx.accounts.market.junior_mint,
                ErrorCode::InvalidLpMintKey
            );
            require!(
                ctx.accounts.market.tranches.junior_reserve_shares > 0
                    && ctx.accounts.market.tranches.junior_receipt_supply > 0,
                ErrorCode::InvalidProposalSupport
            );
            GovernanceTokenKind::Junior
        };
        validate_owner_lp_account(
            ctx.accounts.owner.key(),
            &ctx.accounts.lp_mint,
            &ctx.accounts.owner_lp_account,
        )?;
        let yield_kind = match token_kind {
            GovernanceTokenKind::Senior => YieldTokenKind::Ylp,
            GovernanceTokenKind::Junior => YieldTokenKind::Junior,
        };
        ctx.accounts.base_yield_account.assert_account(
            ctx.accounts.owner.key(),
            ctx.accounts.market.key(),
            ctx.accounts.lp_mint.key(),
            ctx.accounts.market.base_side.asset_mint,
            yield_kind,
        )?;
        ctx.accounts.quote_yield_account.assert_account(
            ctx.accounts.owner.key(),
            ctx.accounts.market.key(),
            ctx.accounts.lp_mint.key(),
            ctx.accounts.market.quote_side.asset_mint,
            yield_kind,
        )?;
        let slot = Clock::get()?.slot;
        crate::instructions::accounting::accrue_market_interest(
            &mut ctx.accounts.market,
            slot,
            ctx.accounts.event_authority.to_account_info(),
        )?;
        if token_kind == GovernanceTokenKind::Junior {
            ctx.accounts
                .market
                .reconcile_junior_receipts(ctx.accounts.lp_mint.supply)?;
        }
        let indexes = carry_forward_governance_yield(&mut ctx.accounts.market, slot)?;
        match token_kind {
            GovernanceTokenKind::Senior => checkpoint_supporter_yield(
                &ctx.accounts.market,
                &mut ctx.accounts.base_yield_account,
                &mut ctx.accounts.quote_yield_account,
                ctx.accounts.owner_lp_account.amount,
                indexes,
            )?,
            GovernanceTokenKind::Junior => {
                for (asset, holder) in [
                    (crate::state::MarketAsset::Base, &mut ctx.accounts.base_yield_account),
                    (crate::state::MarketAsset::Quote, &mut ctx.accounts.quote_yield_account),
                ] {
                    ctx.accounts
                        .market
                        .checkpoint_holder_income(holder, ctx.accounts.owner_lp_account.amount, None)?;
                    let fees = &ctx.accounts.market.side(asset).fees.junior;
                    holder.accrue(
                        ctx.accounts.owner_lp_account.amount,
                        fees.swap_growth_q64,
                        fees.interest_growth_q64,
                    )?;
                }
            }
        }
        let balance = &mut ctx.accounts.governance_balance;
        if balance.market == Pubkey::default() {
            balance.market = ctx.accounts.market.key();
            balance.owner = ctx.accounts.owner.key();
            balance.lp_mint = ctx.accounts.lp_mint.key();
            balance.token_kind = token_kind.code();
            balance.delegate = Pubkey::default();
            balance.total_amount = 0;
            balance.committed_amount = 0;
            balance.base_yield = VirtualYieldLedger::default();
            balance.quote_yield = VirtualYieldLedger::default();
            balance
                .base_yield
                .initialize(indexes.base_swap_fee_q64, indexes.base_interest_q64);
            balance
                .quote_yield
                .initialize(indexes.quote_swap_fee_q64, indexes.quote_interest_q64);
            balance.bump = ctx.bumps.governance_balance;
        }
        balance.assert_account(ctx.accounts.market.key(), balance.key())?;
        require_keys_eq!(balance.owner, ctx.accounts.owner.key(), ErrorCode::InvalidSigner);
        require_keys_eq!(balance.lp_mint, ctx.accounts.lp_mint.key(), ErrorCode::InvalidLpMintKey);
        require_eq!(balance.token_kind, token_kind.code(), ErrorCode::InvalidProposalSupport);
        let total = balance.total_amount;
        if token_kind == GovernanceTokenKind::Senior {
            balance
                .base_yield
                .accrue(total, indexes.base_swap_fee_q64, indexes.base_interest_q64)?;
            balance
                .quote_yield
                .accrue(total, indexes.quote_swap_fee_q64, indexes.quote_interest_q64)?;
            balance.base_yield.drain_into(&mut ctx.accounts.base_yield_account)?;
            balance.quote_yield.drain_into(&mut ctx.accounts.quote_yield_account)?;
        }
        let (action_code, amount) = match action {
            GovernanceBalanceAction::Deposit { amount } => {
                require!(amount > 0, ErrorCode::AmountZero);
                require_gte!(
                    ctx.accounts.owner_lp_account.amount,
                    amount,
                    ErrorCode::InsufficientBalance
                );
                token_burn(
                    ctx.accounts.owner.to_account_info(),
                    ctx.accounts.token_2022_program.to_account_info(),
                    ctx.accounts.lp_mint.to_account_info(),
                    ctx.accounts.owner_lp_account.to_account_info(),
                    amount,
                    &[],
                )?;
                balance.total_amount = balance
                    .total_amount
                    .checked_add(amount)
                    .ok_or(ErrorCode::MarketMathOverflow)?;
                match token_kind {
                    GovernanceTokenKind::Senior => {
                        ctx.accounts.market.governance_locked_ylp = ctx
                            .accounts
                            .market
                            .governance_locked_ylp
                            .checked_add(amount)
                            .ok_or(ErrorCode::MarketMathOverflow)?;
                    }
                    GovernanceTokenKind::Junior => {
                        ctx.accounts.market.governance_locked_junior = ctx
                            .accounts
                            .market
                            .governance_locked_junior
                            .checked_add(amount)
                            .ok_or(ErrorCode::MarketMathOverflow)?;
                    }
                }
                (0, amount)
            }
            GovernanceBalanceAction::Withdraw { amount } => {
                require!(amount > 0, ErrorCode::AmountZero);
                require_gte!(balance.available()?, amount, ErrorCode::InsufficientBalance);
                balance.total_amount -= amount;
                match token_kind {
                    GovernanceTokenKind::Senior => {
                        ctx.accounts.market.governance_locked_ylp = ctx
                            .accounts
                            .market
                            .governance_locked_ylp
                            .checked_sub(amount)
                            .ok_or(ErrorCode::InvalidProposalSupport)?;
                    }
                    GovernanceTokenKind::Junior => {
                        ctx.accounts.market.governance_locked_junior = ctx
                            .accounts
                            .market
                            .governance_locked_junior
                            .checked_sub(amount)
                            .ok_or(ErrorCode::InvalidProposalSupport)?;
                    }
                }
                let seeds = generate_market_seeds!(ctx.accounts.market);
                token_mint_to(
                    ctx.accounts.market.to_account_info(),
                    ctx.accounts.token_2022_program.to_account_info(),
                    ctx.accounts.lp_mint.to_account_info(),
                    ctx.accounts.owner_lp_account.to_account_info(),
                    amount,
                    &[&seeds[..]],
                )?;
                (1, amount)
            }
            GovernanceBalanceAction::ClaimYield => (3, 0),
        };
        if action_code <= 1 {
            super::adjust_supporter_income_balance(
                &mut ctx.accounts.base_yield_account,
                &mut ctx.accounts.quote_yield_account,
                amount,
                action_code == 0,
            )?;
        }
        emit_cpi!(GovernanceBalanceUpdated {
            market: ctx.accounts.market.key(),
            owner: balance.owner,
            governance_balance: balance.key(),
            lp_mint: balance.lp_mint,
            token_kind: balance.token_kind,
            delegate: balance.delegate,
            action: action_code,
            amount,
            total_amount: balance.total_amount,
            committed_amount: balance.committed_amount,
        });
        Ok(())
    }
}

/// Authorization changes do not require token accounts, interest accrual, or
/// settlement. In particular, closing an empty wallet ATA cannot block revocation.
#[event_cpi]
#[derive(Accounts)]
pub struct SetGovernanceDelegate<'info> {
    pub owner: Signer<'info>,
    #[account(mut, seeds = [GOVERNANCE_BALANCE_SEED_PREFIX, governance_balance.market.as_ref(), owner.key().as_ref(), governance_balance.lp_mint.as_ref()], bump = governance_balance.bump)]
    pub governance_balance: Box<Account<'info, GovernanceBalance>>,
}

impl<'info> SetGovernanceDelegate<'info> {
    pub fn handle(ctx: Context<'_, '_, '_, 'info, Self>, delegate: Pubkey) -> Result<()> {
        let balance = &mut ctx.accounts.governance_balance;
        require_keys_eq!(balance.owner, ctx.accounts.owner.key(), ErrorCode::InvalidSigner);
        balance.assert_account(balance.market, balance.key())?;
        balance.delegate = delegate;
        emit_cpi!(GovernanceBalanceUpdated {
            market: balance.market,
            owner: balance.owner,
            governance_balance: balance.key(),
            lp_mint: balance.lp_mint,
            token_kind: balance.token_kind,
            delegate,
            action: 2,
            amount: 0,
            total_amount: balance.total_amount,
            committed_amount: balance.committed_amount
        });
        Ok(())
    }
}
